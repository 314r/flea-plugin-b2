"""An in-memory stand-in for the parts of the B2 Native API the plugin uses.

It keeps file versions like B2 does: a hide adds a version, listings show the latest visible one.
Faults and delays can be injected per operation to exercise retries and the background path.
"""
import base64
import hashlib
import json
import mimetypes
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

API = "/b2api/v4/"


class FakeB2:
    def __init__(self, key_id="test-key-id", key="test-key", bucket="test-bucket",
                 part_size=100_000_000, min_part=5_000_000):
        self.key_id = key_id
        self.key = key
        self.bucket = bucket
        self.bucket_id = "bucket-1"
        # Bucket names to IDs; every file version belongs to one bucket.
        self.buckets = {bucket: self.bucket_id}
        self.part_size = part_size
        self.min_part = min_part
        self.versions = []
        self.large = {}
        self.tokens = set()
        self.upload_tokens = set()
        self.faults = []
        self.delays = {}
        self.calls = []
        self.lock = threading.Lock()
        self.counter = 0
        self.allowed_buckets = None
        handler = type("Handler", (_Handler,), {"fake": self})
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server.server_address[1]}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()

    # Fixtures and inspection.

    def next_id(self, kind="file"):
        with self.lock:
            self.counter += 1
            return f"{kind}-{self.counter}"

    def add_bucket(self, name):
        self.buckets[name] = f"bucket-{len(self.buckets) + 1}"
        return self.buckets[name]

    def put(self, name, data=b"", mtime_ms=None, content_type="application/octet-stream", bucket=None):
        info = {} if mtime_ms is None else {"src_last_modified_millis": str(mtime_ms)}
        return self._add(name, data, content_type, info, bucket_id=self.buckets[bucket or self.bucket])

    def _add(self, name, data, content_type, info, sha1=None, action="upload", bucket_id=None):
        version = {
            "bucketId": bucket_id or self.bucket_id,
            "fileId": self.next_id(),
            "fileName": name,
            "action": action,
            "data": data,
            "contentType": content_type,
            "fileInfo": dict(info),
            "contentSha1": sha1 or hashlib.sha1(data).hexdigest(),
            "uploadTimestamp": int(time.time() * 1000),
        }
        with self.lock:
            self.versions.append(version)
        return version

    def latest(self, bucket_id=None):
        found = {}
        with self.lock:
            for version in self.versions:
                if version["bucketId"] == (bucket_id or self.bucket_id):
                    found[version["fileName"]] = version
        return found

    def visible(self, bucket_id=None):
        return {name: v for name, v in self.latest(bucket_id).items() if v["action"] == "upload"}

    def data(self, name, bucket=None):
        version = self.visible(self.buckets[bucket or self.bucket]).get(name)
        return None if version is None else version["data"]

    def hidden(self, name, bucket=None):
        version = self.latest(self.buckets[bucket or self.bucket]).get(name)
        return version is not None and version["action"] == "hide"

    def fail(self, op, status=503, code="service_unavailable", count=1):
        self.faults.append({"op": op, "status": status, "code": code, "count": count})

    def expire_tokens(self):
        with self.lock:
            self.tokens.clear()
            self.upload_tokens.clear()

    # Request helpers.

    def take_fault(self, op):
        with self.lock:
            for fault in self.faults:
                if fault["op"] == op and fault["count"] > 0:
                    fault["count"] -= 1
                    return fault
        return None

    def file_json(self, v, folder=None):
        if folder is not None:
            return {"action": "folder", "fileName": folder, "fileId": None, "contentLength": 0,
                    "contentSha1": None, "contentType": None, "fileInfo": {}, "uploadTimestamp": 0}
        return {
            "accountId": "account-1",
            "bucketId": v["bucketId"],
            "action": v["action"],
            "fileId": v["fileId"],
            "fileName": v["fileName"],
            "contentLength": len(v["data"]),
            "contentSha1": v["contentSha1"],
            "contentType": v["contentType"],
            "fileInfo": v["fileInfo"],
            "uploadTimestamp": v["uploadTimestamp"],
        }

    def list_names(self, body):
        prefix = body.get("prefix") or ""
        delimiter = body.get("delimiter")
        start = body.get("startFileName")
        count = body.get("maxFileCount") or 100
        if count > 10000:
            return 400, {"status": 400, "code": "out_of_range", "message": "maxFileCount out of range"}
        sequence = []
        folders = set()
        for name, version in sorted(self.visible(body.get("bucketId")).items()):
            if not name.startswith(prefix):
                continue
            rest = name[len(prefix):]
            if delimiter and delimiter in rest:
                folder = prefix + rest[: rest.index(delimiter) + 1]
                if folder not in folders:
                    folders.add(folder)
                    sequence.append((folder, self.file_json(None, folder)))
                continue
            sequence.append((name, self.file_json(version)))
        sequence.sort(key=lambda item: item[0])
        if start:
            sequence = [item for item in sequence if item[0] >= start]
        page = sequence[:count]
        following = sequence[count][0] if len(sequence) > count else None
        return 200, {"files": [item[1] for item in page], "nextFileName": following}

    def api(self, op, body):
        if op == "b2_list_buckets":
            buckets = [{"bucketId": bid, "bucketName": name, "bucketType": "allPrivate"}
                       for name, bid in sorted(self.buckets.items())]
            if body.get("bucketName"):
                buckets = [b for b in buckets if b["bucketName"] == body["bucketName"]]
            return 200, {"buckets": buckets}
        if op == "b2_list_file_names":
            return self.list_names(body)
        if op == "b2_get_upload_url":
            token = self.next_id("upload-token")
            self.upload_tokens.add(token)
            return 200, {"bucketId": body["bucketId"], "uploadUrl": f"{self.url}/upload/{body['bucketId']}",
                         "authorizationToken": token}
        if op == "b2_start_large_file":
            file_id = self.next_id("large")
            self.large[file_id] = {"fileName": body["fileName"], "contentType": body["contentType"],
                                   "bucketId": body["bucketId"],
                                   "fileInfo": body.get("fileInfo") or {}, "parts": {}}
            return 200, {"fileId": file_id, "fileName": body["fileName"], "action": "start"}
        if op == "b2_get_upload_part_url":
            token = self.next_id("part-token")
            self.upload_tokens.add(token)
            return 200, {"fileId": body["fileId"], "uploadUrl": f"{self.url}/upload_part/{body['fileId']}",
                         "authorizationToken": token}
        if op == "b2_finish_large_file":
            large = self.large.pop(body["fileId"], None)
            if large is None:
                return 400, {"status": 400, "code": "bad_request", "message": "no such large file"}
            numbers = sorted(large["parts"])
            if numbers != list(range(1, len(numbers) + 1)) or len(numbers) < 2:
                return 400, {"status": 400, "code": "bad_request", "message": "parts missing"}
            hashes = [large["parts"][n][1] for n in numbers]
            if hashes != body["partSha1Array"]:
                return 400, {"status": 400, "code": "bad_request", "message": "sha1 array mismatch"}
            if any(len(large["parts"][n][0]) < self.min_part for n in numbers[:-1]):
                return 400, {"status": 400, "code": "bad_request", "message": "part too small"}
            data = b"".join(large["parts"][n][0] for n in numbers)
            version = self._add(large["fileName"], data, large["contentType"], large["fileInfo"], sha1="none",
                                bucket_id=large["bucketId"])
            return 200, self.file_json(version)
        if op == "b2_cancel_large_file":
            self.large.pop(body["fileId"], None)
            return 200, {"fileId": body["fileId"]}
        if op == "b2_hide_file":
            version = self.visible(body["bucketId"]).get(body["fileName"])
            if version is None:
                return 404, {"status": 404, "code": "not_found", "message": "file not present"}
            hidden = self._add(body["fileName"], b"", "application/x-bz-hide-marker", {}, action="hide",
                               bucket_id=body["bucketId"])
            return 200, self.file_json(hidden)
        if op == "b2_copy_file":
            source = next((v for v in self.versions if v["fileId"] == body["sourceFileId"]), None)
            if source is None:
                return 404, {"status": 404, "code": "not_found", "message": "source not found"}
            version = self._add(body["fileName"], source["data"], source["contentType"], source["fileInfo"],
                                sha1=source["contentSha1"],
                                bucket_id=body.get("destinationBucketId") or source["bucketId"])
            return 200, self.file_json(version)
        if op == "b2_copy_part":
            source = next((v for v in self.versions if v["fileId"] == body["sourceFileId"]), None)
            large = self.large.get(body["largeFileId"])
            first, last = body["range"][len("bytes="):].split("-")
            data = source["data"][int(first): int(last) + 1]
            sha1 = hashlib.sha1(data).hexdigest()
            large["parts"][body["partNumber"]] = (data, sha1)
            return 200, {"partNumber": body["partNumber"], "contentSha1": sha1, "contentLength": len(data)}
        return 400, {"status": 400, "code": "bad_request", "message": f"unknown op {op}"}


class _Handler(BaseHTTPRequestHandler):
    fake = None
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def answer(self, status, value, headers=None, raw=None):
        body = raw if raw is not None else json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Content-Type", "application/json" if raw is None else "application/octet-stream")
        for key, val in (headers or {}).items():
            self.send_header(key, val)
        self.end_headers()
        self.wfile.write(body)

    def body(self):
        length = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(length) if length else b""

    def gate(self, op, tokens):
        fake = self.fake
        fake.calls.append(op)
        delay = fake.delays.get(op)
        if delay:
            time.sleep(delay)
        fault = fake.take_fault(op)
        if fault:
            self.answer(fault["status"], {"status": fault["status"], "code": fault["code"], "message": fault["code"]})
            return False
        if tokens is not None and self.headers.get("Authorization") not in tokens:
            self.answer(401, {"status": 401, "code": "expired_auth_token", "message": "Authorization token has expired"})
            return False
        return True

    def do_GET(self):
        fake = self.fake
        path = urllib.parse.urlsplit(self.path).path
        if path == API + "b2_authorize_account":
            if not self.gate("b2_authorize_account", None):
                return
            expected = "Basic " + base64.b64encode(f"{fake.key_id}:{fake.key}".encode()).decode()
            if self.headers.get("Authorization") != expected:
                self.answer(401, {"status": 401, "code": "unauthorized", "message": "bad key"})
                return
            token = fake.next_id("token")
            fake.tokens.add(token)
            allowed = {"capabilities": ["listFiles", "readFiles", "writeFiles", "listBuckets"],
                       "buckets": fake.allowed_buckets, "namePrefix": None}
            self.answer(200, {
                "accountId": "account-1",
                "authorizationToken": token,
                "apiInfo": {"storageApi": {
                    "apiUrl": fake.url, "downloadUrl": fake.url,
                    "recommendedPartSize": str(fake.part_size),
                    "absoluteMinimumPartSize": fake.min_part, "allowed": allowed}},
            })
            return
        if path.startswith("/file/"):
            if not self.gate("download", fake.tokens):
                return
            bucket, _, name = path[len("/file/"):].partition("/")
            name = urllib.parse.unquote(name)
            bucket_id = fake.buckets.get(urllib.parse.unquote(bucket))
            version = fake.visible(bucket_id).get(name) if bucket_id else None
            if version is None:
                self.answer(404, {"status": 404, "code": "not_found", "message": "not found"})
                return
            headers = {"X-Bz-File-Id": version["fileId"], "X-Bz-Content-Sha1": version["contentSha1"],
                       "X-Bz-Upload-Timestamp": str(version["uploadTimestamp"])}
            for key, value in version["fileInfo"].items():
                headers["X-Bz-Info-" + key] = urllib.parse.quote(value)
            self.answer(200, None, headers, raw=version["data"])
            return
        self.answer(404, {"status": 404, "code": "not_found", "message": path})

    def do_POST(self):
        fake = self.fake
        path = urllib.parse.urlsplit(self.path).path
        if path.startswith(API):
            op = path[len(API):]
            raw = self.body()
            if not self.gate(op, fake.tokens):
                return
            status, value = fake.api(op, json.loads(raw or b"{}"))
            self.answer(status, value)
            return
        if path.startswith("/upload/"):
            data = self.body()
            if not self.gate("upload", fake.upload_tokens):
                return
            sha1 = self.headers.get("X-Bz-Content-Sha1")
            if sha1 != hashlib.sha1(data).hexdigest():
                self.answer(400, {"status": 400, "code": "bad_request", "message": "Sha1 did not match data received"})
                return
            name = urllib.parse.unquote(self.headers["X-Bz-File-Name"])
            content_type = self.headers.get("Content-Type")
            if content_type == "b2/x-auto":
                content_type = mimetypes.guess_type(name)[0] or "application/octet-stream"
            info = {k[len("X-Bz-Info-"):].lower(): urllib.parse.unquote(v)
                    for k, v in self.headers.items() if k.lower().startswith("x-bz-info-")}
            version = fake._add(name, data, content_type, info, bucket_id=path[len("/upload/"):])
            self.answer(200, fake.file_json(version))
            return
        if path.startswith("/upload_part/"):
            data = self.body()
            if not self.gate("upload_part", fake.upload_tokens):
                return
            file_id = path[len("/upload_part/"):]
            sha1 = self.headers.get("X-Bz-Content-Sha1")
            if sha1 != hashlib.sha1(data).hexdigest():
                self.answer(400, {"status": 400, "code": "bad_request", "message": "Sha1 did not match data received"})
                return
            number = int(self.headers["X-Bz-Part-Number"])
            fake.large[file_id]["parts"][number] = (data, sha1)
            self.answer(200, {"fileId": file_id, "partNumber": number, "contentLength": len(data), "contentSha1": sha1})
            return
        self.answer(404, {"status": 404, "code": "not_found", "message": path})
