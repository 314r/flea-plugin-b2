//! A small Backblaze B2 Native API client.
//!
//! It follows B2's integration rules: authorize again on an expired token, take a fresh upload
//! URL after any upload failure, and back off on 408, 429 and 5xx answers.
use crate::error::{Error, Result};
use crate::util::{hex, hook, int, text};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use ring::digest::{Context, SHA1_FOR_LEGACY_USE_ONLY};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const AUTH_URL: &str = "https://api.backblazeb2.com";
const API: &str = "/b2api/v4/";
const ATTEMPTS: usize = 5;
const CHUNK: usize = 1 << 16;
/// A single upload or b2_copy_file holds at most 5 GB; bigger files go in parts.
pub const SINGLE_MAX: u64 = 5_000_000_000;
const MAX_PARTS: u64 = 10_000;
pub const EMPTY_SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";

/// The characters B2 wants percent-encoded in a URL segment: all but unreserved ones.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');
/// The same in a file name, which keeps its '/'.
const NAME: &AsciiSet = &SEGMENT.remove(b'/');

pub fn quote(name: &str) -> String {
    utf8_percent_encode(name, NAME).to_string()
}

fn agent() -> ureq::Agent {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    ureq::AgentBuilder::new()
        .tls_config(Arc::new(config))
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(120))
        .user_agent(concat!("flea-b2/", env!("CARGO_PKG_VERSION"), "+rust"))
        .build()
}

fn b2_error(status: u16, response: ureq::Response) -> Error {
    let retry_after = response
        .header("retry-after")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|v| v.min(60.0));
    let body: Value = response
        .into_string()
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let code = match text(&body, "code") {
        "" => format!("http_{status}"),
        code => code.to_string(),
    };
    let message = match text(&body, "message") {
        "" => format!("Backblaze B2 answered {status} ({code})."),
        message => message.to_string(),
    };
    Error::B2 {
        status,
        code,
        message,
        retry_after,
    }
}

/// The outcome of a request: the response, or a classified error.
fn sent(result: std::result::Result<ureq::Response, ureq::Error>) -> Result<ureq::Response> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(status, response)) => Err(b2_error(status, response)),
        Err(ureq::Error::Transport(transport)) => Err(Error::Network(format!(
            "Could not reach Backblaze B2: {transport}"
        ))),
    }
}

fn json(result: std::result::Result<ureq::Response, ureq::Error>) -> Result<Value> {
    let response = sent(result)?;
    let value: Value = serde_json::from_reader(response.into_reader())
        .map_err(|e| Error::Network(format!("Backblaze B2 sent an unreadable answer: {e}")))?;
    if value.is_object() || value.is_array() {
        Ok(value)
    } else {
        Err(Error::Network(
            "Backblaze B2 sent an unreadable answer.".into(),
        ))
    }
}

/// Sends one byte range of a file and reports how much has gone out. A local read failure is
/// remembered, so it is reported as such instead of as a network error.
struct Reader<'a> {
    file: File,
    left: u64,
    sent: u64,
    progress: &'a mut dyn FnMut(u64),
    failure: Option<String>,
    path: String,
}

impl Read for Reader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let want = buf.len().min(self.left as usize);
        let n = match self.file.read(&mut buf[..want]) {
            Ok(0) => {
                let text = format!("{} became shorter while it was uploaded.", self.path);
                self.failure = Some(text.clone());
                return Err(std::io::Error::other(text));
            }
            Ok(n) => n,
            Err(error) => {
                self.failure = Some(format!("Could not read {}: {error}", self.path));
                return Err(error);
            }
        };
        self.left -= n as u64;
        self.sent += n as u64;
        (self.progress)(self.sent);
        Ok(n)
    }
}

/// The non-empty `field` of every bucket in a list.
fn names(buckets: Option<&Value>, field: &str) -> Vec<String> {
    let list = buckets.and_then(Value::as_array).into_iter().flatten();
    list.map(|b| text(b, field).to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

/// A listed file's size.
pub fn size(file: &Value) -> u64 {
    int(file.get("contentLength")).unwrap_or(0).max(0) as u64
}

fn open(path: &Path) -> Result<File> {
    File::open(path).map_err(|e| Error::Local(format!("Could not open {}: {e}", path.display())))
}

pub struct Client {
    agent: ureq::Agent,
    key_id: String,
    key: String,
    auth_url: String,
    /// A time past which no retry starts, so a listing stays inside Flea's deadline.
    pub deadline: Option<Instant>,
    token: String,
    pub account_id: String,
    api_url: String,
    download_url: String,
    part_size: u64,
    min_part_size: u64,
    pub allowed: Value,
    /// Upload URLs and tokens by bucket ID, and part upload URLs by large file ID.
    upload_urls: HashMap<String, (String, String)>,
}

impl Client {
    pub fn new(key_id: String, key: String) -> Self {
        Client {
            agent: agent(),
            key_id,
            key,
            auth_url: hook("FLEA_B2_API_URL")
                .unwrap_or_else(|| AUTH_URL.into())
                .trim_end_matches('/')
                .into(),
            deadline: None,
            token: String::new(),
            account_id: String::new(),
            api_url: String::new(),
            download_url: String::new(),
            part_size: 100_000_000,
            min_part_size: 5_000_000,
            allowed: Value::Null,
            upload_urls: HashMap::new(),
        }
    }

    // Authorization and the generic call.

    pub fn authorize(&mut self) -> Result<()> {
        use base64::Engine;
        let basic = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.key_id, self.key));
        let url = format!("{}{API}b2_authorize_account", self.auth_url);
        let data = self
            .retry(false, false, |c| {
                json(c.agent.get(&url).set("Authorization", &format!("Basic {basic}")).timeout(c.timeout(None)).call())
            })
            .map_err(|error| match error {
                Error::B2 { status: 401, ref code, .. } if code == "unauthorized" || code == "bad_auth_token" => Error::B2 {
                    status: 401,
                    code: code.clone(),
                    message: "Backblaze rejected the key ID or application key. Check them, or give the bucket another key under “Manage buckets…”.".into(),
                    retry_after: None,
                },
                other => other,
            })?;
        let storage = data.pointer("/apiInfo/storageApi").unwrap_or(&data);
        self.account_id = text(&data, "accountId").into();
        self.token = text(&data, "authorizationToken").into();
        self.api_url = text(storage, "apiUrl").trim_end_matches('/').into();
        self.download_url = text(storage, "downloadUrl").trim_end_matches('/').into();
        self.part_size = int(storage.get("recommendedPartSize"))
            .filter(|n| *n > 0)
            .unwrap_or(100_000_000) as u64;
        self.min_part_size = int(storage.get("absoluteMinimumPartSize"))
            .filter(|n| *n > 0)
            .unwrap_or(5_000_000) as u64;
        self.allowed = storage.get("allowed").cloned().unwrap_or(Value::Null);
        if self.token.is_empty() || self.api_url.is_empty() || self.download_url.is_empty() {
            return Err(Error::Network(
                "Backblaze B2 sent an incomplete authorization.".into(),
            ));
        }
        self.upload_urls.clear();
        Ok(())
    }

    fn timeout(&self, wanted: Option<Duration>) -> Duration {
        let wanted = wanted.unwrap_or(Duration::from_secs(10));
        match self.deadline {
            None => wanted,
            Some(deadline) => wanted
                .min(deadline.saturating_duration_since(Instant::now()))
                .max(Duration::from_secs(1)),
        }
    }

    fn retry<T>(
        &mut self,
        reauthorize: bool,
        upload: bool,
        mut attempt: impl FnMut(&mut Self) -> Result<T>,
    ) -> Result<T> {
        let mut delay = 1.0f64;
        let mut renewed = false;
        for number in 0..ATTEMPTS {
            let last = number + 1 == ATTEMPTS;
            let error = match attempt(self) {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            let expired = matches!(&error, Error::B2 { status: 401, code, .. }
                if code == "expired_auth_token" || code == "bad_auth_token");
            let wait = match &error {
                // The attempt dropped its upload URL; the next one asks for a new one.
                _ if expired && upload && !last => 0.0,
                _ if expired && reauthorize && !renewed => {
                    self.authorize()?;
                    renewed = true;
                    continue;
                }
                Error::B2 {
                    status,
                    retry_after,
                    ..
                } if matches!(status, 408 | 429 | 500 | 502 | 503 | 504) && !last => {
                    retry_after.unwrap_or(delay)
                }
                Error::Network(_) if !last => delay,
                _ => return Err(error),
            };
            if let Some(deadline) = self.deadline {
                if Instant::now() + Duration::from_secs_f64(wait) > deadline {
                    return Err(error);
                }
            }
            std::thread::sleep(Duration::from_secs_f64(wait));
            delay = (delay * 2.0).min(16.0);
        }
        Err(Error::Network("Backblaze B2 kept failing.".into()))
    }

    pub fn call(
        &mut self,
        operation: &str,
        body: Value,
        timeout: Option<Duration>,
    ) -> Result<Value> {
        let body = match body {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .filter(|(_, v)| !v.is_null())
                    .collect::<Map<_, _>>(),
            ),
            other => other,
        };
        let bytes = body.to_string().into_bytes();
        self.retry(true, false, |c| {
            let url = format!("{}{API}{operation}", c.api_url);
            json(
                c.agent
                    .post(&url)
                    .set("Authorization", &c.token)
                    .set("Content-Type", "application/json")
                    .timeout(c.timeout(timeout))
                    .send_bytes(&bytes),
            )
        })
    }

    // Buckets and names.

    pub fn bucket_id(&mut self, name: &str) -> Result<String> {
        if let Some(buckets) = self.allowed.get("buckets").and_then(Value::as_array) {
            if let Some(bucket) = buckets
                .iter()
                .find(|b| text(b, "name") == name && !text(b, "id").is_empty())
            {
                return Ok(text(bucket, "id").into());
            }
            if !buckets.is_empty() {
                return Err(Error::msg(format!(
                    "This key cannot open the bucket “{name}”."
                )));
            }
        }
        if text(&self.allowed, "bucketName") == name && !text(&self.allowed, "bucketId").is_empty()
        {
            return Ok(text(&self.allowed, "bucketId").into());
        }
        let answer = self.call(
            "b2_list_buckets",
            json!({"accountId": self.account_id, "bucketName": name, "bucketTypes": ["all"]}),
            None,
        )?;
        answer
            .get("buckets")
            .and_then(Value::as_array)
            .and_then(|list| list.iter().find(|b| text(b, "bucketName") == name))
            .map(|b| text(b, "bucketId").to_string())
            .ok_or_else(|| {
                Error::msg(format!(
                    "The bucket “{name}” does not exist, or this key cannot see it."
                ))
            })
    }

    /// The names of the buckets this key can open.
    pub fn bucket_names(&mut self) -> Result<Vec<String>> {
        let restricted = names(self.allowed.get("buckets"), "name");
        if !restricted.is_empty() {
            return Ok(restricted);
        }
        if !text(&self.allowed, "bucketName").is_empty() {
            return Ok(vec![text(&self.allowed, "bucketName").into()]);
        }
        let answer = self.call(
            "b2_list_buckets",
            json!({"accountId": self.account_id, "bucketTypes": ["all"]}),
            None,
        )?;
        Ok(names(answer.get("buckets"), "bucketName"))
    }

    pub fn list_names(
        &mut self,
        bucket_id: &str,
        prefix: &str,
        delimiter: Option<&str>,
        start: Option<&str>,
        count: u32,
    ) -> Result<(Vec<Value>, Option<String>)> {
        let answer = self.call(
            "b2_list_file_names",
            json!({"bucketId": bucket_id, "prefix": prefix, "delimiter": delimiter,
                   "startFileName": start, "maxFileCount": count}),
            None,
        )?;
        let files = match answer.get("files") {
            Some(Value::Array(files)) => files.clone(),
            _ => {
                return Err(Error::Network(
                    "Backblaze B2 sent an unreadable file list.".into(),
                ))
            }
        };
        let next = answer
            .get("nextFileName")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from);
        Ok((files, next))
    }

    /// Every visible file under a prefix, at any depth.
    pub fn all_files(&mut self, bucket_id: &str, prefix: &str) -> Result<Vec<Value>> {
        let mut found = Vec::new();
        let mut start: Option<String> = None;
        loop {
            let (files, next) = self.list_names(bucket_id, prefix, None, start.as_deref(), 1000)?;
            found.extend(files.into_iter().filter(|f| {
                text(f, "action") == "upload" && text(f, "fileName").starts_with(prefix)
            }));
            match next {
                Some(next) => start = Some(next),
                None => return Ok(found),
            }
        }
    }

    /// The visible file with exactly this name, if any.
    pub fn find_file(&mut self, bucket_id: &str, key: &str) -> Result<Option<Value>> {
        let (files, _) = self.list_names(bucket_id, key, None, Some(key), 1)?;
        Ok(files
            .into_iter()
            .find(|f| text(f, "fileName") == key && text(f, "action") == "upload"))
    }

    pub fn folder_exists(&mut self, bucket_id: &str, prefix: &str) -> Result<bool> {
        Ok(!self
            .list_names(bucket_id, prefix, None, None, 1)?
            .0
            .is_empty())
    }

    // Changes.

    pub fn hide(&mut self, bucket_id: &str, key: &str) -> Result<()> {
        match self.call(
            "b2_hide_file",
            json!({"bucketId": bucket_id, "fileName": key}),
            None,
        ) {
            // Already gone, or already hidden: the end state is the one asked for.
            Ok(_) | Err(Error::B2 { status: 404, .. }) => Ok(()),
            Err(Error::B2 {
                status: 400,
                ref message,
                ..
            }) if message.to_lowercase().contains("hidden") => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// A server-side copy of a listed file under a new name, in the same bucket or another one the
    /// key can write; no data passes through here.
    pub fn copy(
        &mut self,
        source: &Value,
        key: &str,
        bucket_id: &str,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        let size = size(source);
        let source_id = text(source, "fileId").to_string();
        if size <= SINGLE_MAX {
            self.call(
                "b2_copy_file",
                json!({"sourceFileId": source_id, "destinationBucketId": bucket_id, "fileName": key, "metadataDirective": "COPY"}),
                Some(Duration::from_secs(900)),
            )?;
            progress(size);
            return Ok(());
        }
        let part = self.part_size_for(size);
        let content_type = match text(source, "contentType") {
            "" => "b2/x-auto",
            other => other,
        };
        let started = self.call(
            "b2_start_large_file",
            json!({"bucketId": bucket_id, "fileName": key, "contentType": content_type,
                   "fileInfo": source.get("fileInfo").cloned().unwrap_or(json!({}))}),
            None,
        )?;
        let file_id = text(&started, "fileId").to_string();
        let result = (|| {
            let mut hashes = Vec::new();
            let mut offset = 0u64;
            let mut number = 1u64;
            while offset < size {
                let end = (offset + part).min(size) - 1;
                let answer = self.call(
                    "b2_copy_part",
                    json!({"sourceFileId": source_id, "largeFileId": file_id, "partNumber": number,
                           "range": format!("bytes={offset}-{end}")}),
                    Some(Duration::from_secs(900)),
                )?;
                hashes.push(Value::String(text(&answer, "contentSha1").into()));
                progress(end + 1);
                offset = end + 1;
                number += 1;
            }
            self.call(
                "b2_finish_large_file",
                json!({"fileId": file_id, "partSha1Array": hashes}),
                Some(Duration::from_secs(300)),
            )
        })();
        if result.is_err() {
            self.cancel(&file_id);
        }
        result.map(|_| ())
    }

    fn part_size_for(&self, size: u64) -> u64 {
        let wanted = hook("FLEA_B2_PART_SIZE")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|n| *n > 0);
        let part = wanted.unwrap_or_else(|| self.part_size.max(self.min_part_size));
        part.max(size.div_ceil(MAX_PARTS)).min(SINGLE_MAX)
    }

    /// The part size when a file this big goes up in parts, else None.
    pub fn large_part(&self, size: u64) -> Option<u64> {
        let part = self.part_size_for(size);
        (size > part).then_some(part)
    }

    fn cancel(&mut self, file_id: &str) {
        let _ = self.call(
            "b2_cancel_large_file",
            json!({"fileId": file_id}),
            Some(Duration::from_secs(60)),
        );
    }

    // Uploads.

    /// The URL and token to upload through, kept until an upload through it fails.
    fn upload_url(&mut self, operation: &str, field: &str, id: &str) -> Result<(String, String)> {
        if let Some(found) = self.upload_urls.get(id) {
            return Ok(found.clone());
        }
        let answer = self.call(operation, json!({ field: id }), None)?;
        let found = (
            text(&answer, "uploadUrl").to_string(),
            text(&answer, "authorizationToken").to_string(),
        );
        self.upload_urls.insert(id.into(), found.clone());
        Ok(found)
    }

    /// Streams one byte range of a local file to an upload URL.
    fn post_range(
        &self,
        url: &str,
        headers: &[(&str, String)],
        path: &Path,
        offset: u64,
        length: u64,
        progress: &mut dyn FnMut(u64),
    ) -> Result<Value> {
        let mut file = open(path)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| Error::Local(format!("Could not read {}: {e}", path.display())))?;
        let mut reader = Reader {
            file,
            left: length,
            sent: 0,
            progress,
            failure: None,
            path: path.display().to_string(),
        };
        let mut request = self
            .agent
            .post(url)
            .set("Content-Length", &length.to_string());
        for (name, value) in headers {
            request = request.set(name, value);
        }
        let result = json(request.send(&mut reader));
        if let Some(local) = reader.failure {
            return Err(Error::Local(local));
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upload_file(
        &mut self,
        bucket_id: &str,
        key: &str,
        path: &Path,
        size: u64,
        sha1: &str,
        mtime_ms: i64,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        self.retry(true, true, |c| {
            let (url, token) = c.upload_url("b2_get_upload_url", "bucketId", bucket_id)?;
            let headers = [
                ("Authorization", token),
                ("X-Bz-File-Name", quote(key)),
                ("Content-Type", "b2/x-auto".to_string()),
                ("X-Bz-Content-Sha1", sha1.to_string()),
                ("X-Bz-Info-src_last_modified_millis", mtime_ms.to_string()),
            ];
            let result = c.post_range(&url, &headers, path, 0, size, progress);
            if result.is_err() {
                c.upload_urls.remove(bucket_id);
            }
            result
        })
        .map(|_| ())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upload_large(
        &mut self,
        bucket_id: &str,
        key: &str,
        path: &Path,
        size: u64,
        part: u64,
        hashes: &[String],
        whole: &str,
        mtime_ms: i64,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        let started = self.call(
            "b2_start_large_file",
            json!({"bucketId": bucket_id, "fileName": key, "contentType": "b2/x-auto",
                   "fileInfo": {"src_last_modified_millis": mtime_ms.to_string(), "large_file_sha1": whole}}),
            None,
        )?;
        let file_id = text(&started, "fileId").to_string();
        let result = (|| {
            for (index, sha1) in hashes.iter().enumerate() {
                let offset = index as u64 * part;
                let length = part.min(size - offset);
                let mut report = |sent: u64| progress(offset + sent);
                self.retry(true, true, |c| {
                    let (url, token) =
                        c.upload_url("b2_get_upload_part_url", "fileId", &file_id)?;
                    let headers = [
                        ("Authorization", token),
                        ("X-Bz-Part-Number", (index + 1).to_string()),
                        ("X-Bz-Content-Sha1", sha1.clone()),
                    ];
                    let result = c.post_range(&url, &headers, path, offset, length, &mut report);
                    if result.is_err() {
                        c.upload_urls.remove(&file_id);
                    }
                    result
                })?;
            }
            self.call(
                "b2_finish_large_file",
                json!({"fileId": file_id, "partSha1Array": hashes}),
                Some(Duration::from_secs(300)),
            )
        })();
        self.upload_urls.remove(&file_id);
        if result.is_err() {
            self.cancel(&file_id);
        }
        result.map(|_| ())
    }

    // Downloads.

    /// Streams one file into an open file; returns its response headers and the SHA-1 of what arrived.
    pub fn download(
        &mut self,
        bucket_name: &str,
        key: &str,
        out: &mut File,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(HashMap<String, String>, String)> {
        let bucket = utf8_percent_encode(bucket_name, SEGMENT).to_string();
        self.retry(true, false, |c| {
            let url = format!("{}/file/{bucket}/{}", c.download_url, quote(key));
            out.set_len(0)
                .and_then(|_| out.seek(SeekFrom::Start(0)))
                .map_err(|e| Error::Local(format!("Could not write: {e}")))?;
            let response = sent(c.agent.get(&url).set("Authorization", &c.token).call())?;
            let headers: HashMap<String, String> = response
                .headers_names()
                .into_iter()
                .filter_map(|name| {
                    response
                        .header(&name)
                        .map(|v| (name.to_lowercase(), v.to_string()))
                })
                .collect();
            let expected = headers
                .get("content-length")
                .and_then(|v| v.parse::<u64>().ok());
            let mut body = response.into_reader();
            let mut digest = Context::new(&SHA1_FOR_LEGACY_USE_ONLY);
            let mut buf = vec![0u8; CHUNK];
            let mut received = 0u64;
            loop {
                let n = match body.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(Error::Network(format!("The download broke off: {e}"))),
                };
                out.write_all(&buf[..n])
                    .map_err(|e| Error::Local(format!("Could not write: {e}")))?;
                digest.update(&buf[..n]);
                received += n as u64;
                progress(received);
            }
            if expected.is_some_and(|e| e != received) {
                return Err(Error::Network(
                    "The download broke off before its end.".into(),
                ));
            }
            Ok((headers, hex(digest.finish().as_ref())))
        })
    }
}

/// One pass over a file: its SHA-1 and, with a part size, the SHA-1 of every part.
pub fn hash_file(path: &Path, size: u64, part: Option<u64>) -> Result<(String, Vec<String>)> {
    let mut file = open(path)?;
    let mut whole = Context::new(&SHA1_FOR_LEGACY_USE_ONLY);
    let mut parts = Vec::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut left_total = size;
    while left_total > 0 {
        let length = part.map_or(left_total, |p| p.min(left_total));
        let mut piece = Context::new(&SHA1_FOR_LEGACY_USE_ONLY);
        let mut left = length;
        while left > 0 {
            let want = buf.len().min(left as usize);
            let n = file
                .read(&mut buf[..want])
                .map_err(|e| Error::Local(format!("Could not read {}: {e}", path.display())))?;
            if n == 0 {
                return Err(Error::Local(format!(
                    "{} became shorter while it was read.",
                    path.display()
                )));
            }
            whole.update(&buf[..n]);
            piece.update(&buf[..n]);
            left -= n as u64;
        }
        parts.push(hex(piece.finish().as_ref()));
        left_total -= length;
    }
    Ok((hex(whole.finish().as_ref()), parts))
}
