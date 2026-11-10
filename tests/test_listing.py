import os
import time
import unittest

from support import MANAGER_ROW, MORE_MARKER, PluginCase


class Listing(PluginCase):
    def test_root_and_subfolder(self):
        self.fake.put("notes.txt", b"hello", mtime_ms=1_700_000_000_123)
        self.fake.put("Photos/lake.png", b"png!")
        self.fake.put("Photos/2024/x.jpg", b"jpg")
        self.fake.put("Empty/.bzEmpty")
        self.start()
        root = self.listing("/")
        self.assertEqual(set(root), {"notes.txt", "Photos", "Empty"})
        self.assertEqual(root["notes.txt"], {"name": "notes.txt", "kind": "file", "size": 5, "mtime": 1_700_000_000})
        self.assertEqual(root["Photos"], {"name": "Photos", "kind": "dir"})
        photos = self.listing("/Photos")
        self.assertEqual(set(photos), {"lake.png", "2024"})
        self.assertEqual(photos["lake.png"]["size"], 4)
        self.assertEqual(self.listing("/Empty"), {})
        self.assertEqual(self.listing("/Photos/2024/"), {"x.jpg": self.listing("/Photos/2024")["x.jpg"]})

    def test_folder_of_an_entry_is_its_root(self):
        self.fake.put("backups/a.txt", b"a")
        self.fake.put("other/b.txt", b"b")
        self.start(buckets=[self.entry(folder="/backups/")])
        self.assertEqual(set(self.listing("/")), {"a.txt"})

    def test_hidden_files_are_not_listed(self):
        self.fake.put("gone.txt", b"x")
        self.fake.api("b2_hide_file", {"bucketId": self.fake.bucket_id, "fileName": "gone.txt"})
        self.start()
        self.assertEqual(self.listing("/"), {})

    def test_names_flea_would_refuse_are_skipped(self):
        for name in ["ok.txt", "back\\slash.txt", "rtl‮txt.exe", "x" * 300, "a//double.txt", "/lead.txt"]:
            self.fake.put(name, b"x")
        self.start()
        self.assertEqual(set(self.listing("/")), {"ok.txt", "a"})
        self.assertEqual(self.listing("/a"), {})

    def test_file_and_folder_with_one_name(self):
        self.fake.put("same", b"file")
        self.fake.put("same/inside.txt", b"x")
        self.start()
        self.assertEqual(self.listing("/"), {"same": {"name": "same", "kind": "dir"}})

    def test_big_folder_is_cut_with_marker(self):
        for i in range(1200):
            self.fake.put(f"big/file-{i:05d}.txt", b"x")
        for i in range(3):
            self.fake.put(f"big/zz-folder-{i}/a.txt", b"x")
        self.start()
        rows = self.listing("/big")
        self.assertEqual(len(rows), 256)
        self.assertIn(MORE_MARKER, rows)
        for i in range(3):
            self.assertEqual(rows[f"zz-folder-{i}"]["kind"], "dir")

    def test_long_names_stay_under_64k(self):
        for i in range(256):
            self.fake.put("long/" + f"{i:03d}" + "é" * 120, b"x")
        self.start()
        rows = self.listing("/long")
        self.assertIn(MORE_MARKER, rows)

    def test_exactly_256_rows_has_no_marker(self):
        for i in range(256):
            self.fake.put(f"full/{i:03d}", b"x")
        self.start()
        rows = self.listing("/full")
        self.assertEqual(len(rows), 256)
        self.assertNotIn(MORE_MARKER, rows)


class Buckets(PluginCase):
    def test_place_root_lists_the_entries_in_order(self):
        self.fake.add_bucket("archive")
        self.start(buckets=[self.entry(name="Work"), self.entry(name="Archive", bucket="archive")])
        answer, _ = self.request({"op": "list", "path": "/"})
        self.assertEqual(answer["entries"], [{"name": "Work", "kind": "dir"}, {"name": "Archive", "kind": "dir"}])
        self.assertEqual(self.fake.calls, [], "the root needs no network")

    def test_sidebar_rows(self):
        self.start(buckets=[self.entry(name="Work"), self.entry(name="Photos 2024", folder="photos")])
        answer, _ = self.request({"op": "section"})
        self.assertEqual(answer, {"op": "section", "rows": [
            {"id": "Work", "label": "Work", "glyph": "cloud", "opens": "plugin://b2.bucket/Work"},
            {"id": "Photos 2024", "label": "Photos 2024", "glyph": "cloud", "opens": "plugin://b2.bucket/Photos 2024"},
            MANAGER_ROW,
        ]})

    def test_no_entry_yet(self):
        self.start(buckets=[])
        answer, _ = self.request({"op": "list", "path": "/"})
        self.assertEqual(answer["op"], "error")
        self.assertIn("Manage buckets", answer["message"])
        answer, _ = self.request({"op": "section"})
        self.assertEqual(answer, {"op": "section", "rows": [MANAGER_ROW]})

    def test_changes_apply_without_restart(self):
        self.fake.put("a.txt", b"a")
        self.start(buckets=[])
        self.assertEqual(self.request({"op": "section"})[0]["rows"], [MANAGER_ROW])
        self.write_config([self.entry(name="Late")])
        # A distinct modification time, whatever the filesystem's timestamp resolution.
        os.utime(os.path.join(self.env["XDG_CONFIG_HOME"], "flea-b2", "buckets.json"), (time.time() + 5,) * 2)
        self.assertEqual([r["id"] for r in self.request({"op": "section"})[0]["rows"]], ["Late", ".manage"])
        self.assertEqual(set(self.listing("/Late", raw=True)), {"a.txt"})

    def test_removed_entry(self):
        self.start()
        answer, _ = self.request({"op": "list", "path": "/Gone/x"})
        self.assertEqual(answer["op"], "error")
        self.assertIn("no longer", answer["message"])

    def test_unusable_entries_are_skipped(self):
        self.start(buckets=[self.entry(name="a/b"), self.entry(name="Good"), self.entry(name="Good"),
                            {"name": "NoKey", "bucket": "x", "folder": ""}])
        self.assertEqual([r["id"] for r in self.request({"op": "section"})[0]["rows"]], ["Good", ".manage"])

    def test_broken_config_file(self):
        self.start()
        path = os.path.join(self.env["XDG_CONFIG_HOME"], "flea-b2", "buckets.json")
        with open(path, "w") as out:
            out.write("{not json")
        os.utime(path, (time.time() + 5,) * 2)
        answer, _ = self.request({"op": "list", "path": "/"})
        self.assertEqual(answer["op"], "error")
        self.assertIn("not valid JSON", answer["message"])
        self.assertEqual(self.request({"op": "section"})[0], {"op": "section", "rows": [MANAGER_ROW]})


class Manager(PluginCase):
    """The sidebar's "Manage buckets…" row: listing it starts the manager through systemd-run."""

    def fake_launcher(self, status=0, stderr=""):
        """A stand-in for systemd-run that records its arguments, one per line."""
        self.launch_log = os.path.join(self.tmp, "launch.log")
        script = os.path.join(self.tmp, "systemd-run")
        with open(script, "w") as out:
            out.write("#!/bin/sh\n"
                      f"printf '%s\\n' \"$@\" >> '{self.launch_log}'\n"
                      f"printf '%s' '{stderr}' >&2\n"
                      f"exit {status}\n")
        os.chmod(script, 0o755)
        manager = os.path.join(self.tmp, "flea-b2-manager")
        with open(manager, "w") as out:
            out.write("#!/bin/sh\n")
        os.chmod(manager, 0o755)
        return {"FAKE_SYSTEMD_RUN": script, "FAKE_MANAGER_BIN": manager}

    def launched(self):
        """The arguments systemd-run was given, or [] when it was not run."""
        if not os.path.exists(self.launch_log):
            return []
        with open(self.launch_log) as f:
            return f.read().splitlines()

    def test_the_row_starts_the_manager(self):
        self.start(**self.fake_launcher(), WAYLAND_DISPLAY="wayland-test")
        answer, _ = self.request({"op": "list", "path": "/.manage"})
        self.assertEqual(answer, {"op": "list", "entries": []})
        self.assertEqual(self.fake.calls, [], "opening the manager needs no network")
        lines = self.launched()
        self.assertIn("--user", lines)
        self.assertIn("--no-block", lines)
        self.assertIn("--setenv=XDG_CONFIG_HOME", lines)
        self.assertIn("--setenv=WAYLAND_DISPLAY", lines)
        self.assertNotIn("--setenv=LC_ALL", lines, "Flea's LC_ALL=C is not passed on")
        self.assertEqual(lines[lines.index("--") + 1], os.path.join(self.tmp, "flea-b2-manager"))

    def test_a_failed_start_is_reported(self):
        self.start(**self.fake_launcher(status=1, stderr="Failed to connect to user bus"))
        answer, _ = self.request({"op": "list", "path": "/.manage"})
        self.assertEqual(answer["op"], "error")
        self.assertIn("Failed to connect to user bus", answer["message"])

    def test_a_missing_manager_is_reported(self):
        env = self.fake_launcher()
        env["FAKE_MANAGER_BIN"] = os.path.join(self.tmp, "nothing-here")
        self.start(**env)
        answer, _ = self.request({"op": "list", "path": "/.manage"})
        self.assertEqual(answer["op"], "error")
        self.assertIn("missing", answer["message"])
        self.assertEqual(self.launched(), [])

    def test_the_row_is_not_a_bucket(self):
        self.start()
        answer, _ = self.request({"op": "list", "path": "/"})
        self.assertNotIn(".manage", [e["name"] for e in answer["entries"]])


class Keys(PluginCase):
    def test_key_missing_from_the_keyring(self):
        self.start(buckets=[self.entry(key_id="forgotten")])
        answer, _ = self.request({"op": "list", "path": self.p("/")})
        self.assertEqual(answer["op"], "error")
        self.assertIn("forgotten", answer["message"])
        self.assertIn("keyring", answer["message"])

    def test_wrong_key(self):
        self.start(FAKE_B2_KEYS=f"{self.fake.key_id}:wrong")
        answer, _ = self.request({"op": "list", "path": self.p("/")})
        self.assertEqual(answer["op"], "error")
        self.assertIn("rejected", answer["message"])

    def test_unknown_bucket(self):
        self.start(buckets=[self.entry(bucket="nope")])
        answer, _ = self.request({"op": "list", "path": self.p("/")})
        self.assertEqual(answer["op"], "error")
        self.assertIn("nope", answer["message"])

    def test_restricted_key_to_other_bucket(self):
        self.fake.allowed_buckets = [{"id": "bucket-9", "name": "elsewhere"}]
        self.start()
        answer, _ = self.request({"op": "list", "path": self.p("/")})
        self.assertEqual(answer["op"], "error")
        self.assertIn("cannot open", answer["message"])

    def test_cancel_stops_the_process(self):
        self.start()
        self.process.stdin.write(b'{"op":"cancel"}\n')
        self.process.stdin.flush()
        self.assertEqual(self.process.wait(timeout=5), 0)


class Resilience(PluginCase):
    def test_retries_a_busy_service(self):
        self.fake.put("a.txt", b"a")
        self.fake.fail("b2_list_file_names", 503, count=2)
        self.start()
        self.assertEqual(set(self.listing("/")), {"a.txt"})

    def test_authorizes_again_when_the_token_expires(self):
        self.fake.put("a.txt", b"a")
        self.start()
        self.assertEqual(set(self.listing("/")), {"a.txt"})
        self.fake.expire_tokens()
        self.assertEqual(set(self.listing("/")), {"a.txt"})
        self.assertGreaterEqual(self.fake.calls.count("b2_authorize_account"), 2)

    def test_lasting_failure_is_an_error_answer_not_a_crash(self):
        self.fake.fail("b2_list_file_names", 500, code="internal_error", count=100)
        self.start()
        answer, _ = self.request({"op": "list", "path": self.p("/")})
        self.assertEqual(answer["op"], "error")
        answer, _ = self.request({"op": "list", "path": self.p("/")})
        self.assertEqual(answer["op"], "error")


if __name__ == "__main__":
    unittest.main()
