"""Runs bin/run the way Flea does, against the fake B2, with fake keyring and notifications."""
import json
import os
import select
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import unicodedata

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
# A binary built with the test hooks: `cargo build --features test-hooks` (see the Makefile).
RUN = os.environ.get("FLEA_B2_BIN") or os.path.join(ROOT, ".target", "debug", "flea-b2")
sys.path.insert(0, HERE)
sys.dont_write_bytecode = True
MORE_MARKER = "… more items not shown (Flea lists 256 per folder)"

from fake_b2 import FakeB2  # noqa: E402

HELLO = {"op": "hello", "protocolVersion": 1, "flea": "0.4.0", "language": "en", "decimal": ".",
         "kinds": ["place"]}
BIDI = {"؜", "‎", "‏", *map(chr, range(0x202A, 0x202F)), *map(chr, range(0x2066, 0x206A))}


def check_flea_listing(test, line, answer):
    """The checks Flea's runtime and GUI make on a list answer; failing one fails the plugin."""
    test.assertLessEqual(len(line), 65536)
    entries = answer["entries"]
    test.assertLessEqual(len(entries), 256)
    names = set()
    for entry in entries:
        name = entry["name"]
        test.assertTrue(name and name not in (".", "..") and "/" not in name and "\\" not in name, name)
        test.assertLessEqual(len(name.encode()), 255)
        test.assertFalse(any(unicodedata.category(c) == "Cc" or c in BIDI for c in name), name)
        test.assertNotIn(name, names)
        names.add(name)
        test.assertIn(entry["kind"], ("dir", "file"))
        for key in ("size", "mtime"):
            if key in entry:
                test.assertIsInstance(entry[key], int)


# The sidebar's last row, which opens the bucket manager.
MANAGER_ROW = {"id": ".manage", "label": "Manage buckets…", "glyph": "server", "opens": "plugin://b2.bucket/.manage"}


class PluginCase(unittest.TestCase):
    wait_seconds = "20"
    # The entry tests use unless they write their own: "Test", the fake's bucket, whole.
    ROOT = "/Test"

    def setUp(self):
        if not os.access(RUN, os.X_OK):
            self.fail(f"{RUN} is missing: run `make test`, or `cargo build --features test-hooks`")
        self.fake = FakeB2()
        self.addCleanup(self.fake.close)
        self.tmp = tempfile.mkdtemp(prefix="flea-b2-test-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.local = os.path.join(self.tmp, "local")
        os.mkdir(self.local)
        self.notify_log = os.path.join(self.tmp, "notify.log")
        runtime = os.path.join(self.tmp, "runtime")
        os.mkdir(runtime, 0o700)
        self.env = {
            "PATH": os.environ.get("PATH", "/usr/bin"),
            "HOME": self.tmp,
            "LC_ALL": "C",
            "XDG_RUNTIME_DIR": runtime,
            "XDG_STATE_HOME": os.path.join(self.tmp, "state"),
            "FLEA_B2_API_URL": self.fake.url,
            "FLEA_B2_WAIT_SECONDS": self.wait_seconds,
            "XDG_CONFIG_HOME": os.path.join(self.tmp, "config"),
            "FAKE_B2_KEYS": f"{self.fake.key_id}:{self.fake.key}",
            "FAKE_NOTIFY_LOG": self.notify_log,
            "PYTHONDONTWRITEBYTECODE": "1",
        }
        self.process = None

    def tearDown(self):
        if self.process is not None:
            try:
                self.process.stdin.close()
            except OSError:
                pass
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
            self.process.stdout.close()

    def entry(self, name="Test", bucket="test-bucket", folder="", key_id=None):
        return {"name": name, "bucket": bucket, "folder": folder, "keyId": key_id or self.fake.key_id}

    def write_config(self, buckets):
        folder = os.path.join(self.env["XDG_CONFIG_HOME"], "flea-b2")
        os.makedirs(folder, exist_ok=True)
        with open(os.path.join(folder, "buckets.json"), "w") as out:
            json.dump({"version": 1, "buckets": buckets}, out)

    def config(self):
        with open(os.path.join(self.env["XDG_CONFIG_HOME"], "flea-b2", "buckets.json")) as f:
            return json.load(f)["buckets"]

    def start(self, buckets=None, **env):
        """Starts the plugin with these bucket entries, by default one "Test" entry."""
        self.env.update(env)
        self.write_config([self.entry()] if buckets is None else buckets)
        # Its own process group, as Flea starts it: a test may kill that group.
        self.process = subprocess.Popen([RUN], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        env=self.env, cwd=ROOT, process_group=0)
        answer, _ = self.request({**HELLO, "settings": {}})
        self.assertEqual(answer, {"op": "ready", "protocolVersion": 1})

    def p(self, path="/"):
        """A place path inside the default entry: p("/a.txt") is "/Test/a.txt"."""
        return self.ROOT + ("" if path == "/" else path)

    def read(self, timeout=30):
        ready, _, _ = select.select([self.process.stdout], [], [], timeout)
        self.assertTrue(ready, "the plugin did not answer within Flea's 30 seconds")
        line = self.process.stdout.readline()
        self.assertTrue(line.endswith(b"\n"), "the plugin closed its output")
        return line

    def request(self, message):
        """Sends one request; returns the final answer and the progress lines before it."""
        started = time.monotonic()
        self.process.stdin.write((json.dumps(message) + "\n").encode())
        self.process.stdin.flush()
        progress = []
        while True:
            line = self.read()
            answer = json.loads(line)
            self.assertLessEqual(len(line), 65536)
            if answer.get("op") == "progress":
                self.assertLessEqual(answer["done"], answer["total"])
                progress.append(answer)
                continue
            self.assertLess(time.monotonic() - started, 30)
            self.assertLessEqual(len(progress), 128)
            if answer.get("op") == "list":
                check_flea_listing(self, line, answer)
            return answer, progress

    def listing(self, path="/", raw=False):
        """The rows of a folder of the default entry, or of a raw place path."""
        answer, _ = self.request({"op": "list", "path": path if raw else self.p(path)})
        self.assertEqual(answer["op"], "list", answer)
        return {entry["name"]: entry for entry in answer["entries"]}

    def notifications(self):
        try:
            with open(self.notify_log) as f:
                return f.read().splitlines()
        except FileNotFoundError:
            return []

    def write_local(self, relative, data=b"data", mtime=None):
        path = os.path.join(self.local, relative)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as out:
            out.write(data)
        if mtime is not None:
            os.utime(path, (mtime, mtime))
        return path
