import os
import time
import unittest

from support import MORE_MARKER, PluginCase


def transfer(op, paths, dest, source_local, destination_local):
    return {"op": op, "paths": paths, "dest": dest, "destinationLocal": destination_local,
            "sourceLocal": source_local, "overwrite": False}


class Upload(PluginCase):
    def test_file_and_folder(self):
        a = self.write_local("a.txt", b"alpha", mtime=1_600_000_000)
        self.write_local("tree/one.txt", b"one")
        self.write_local("tree/sub/two.txt", b"two")
        os.makedirs(os.path.join(self.local, "tree", "empty"))
        os.symlink("/etc/hostname", os.path.join(self.local, "tree", "link"))
        self.fake.put("Inbox/.bzEmpty")
        self.start()
        answer, _ = self.request(transfer("copy", [a, os.path.join(self.local, "tree")], self.p("/Inbox"), True, False))
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("Inbox/a.txt"), b"alpha")
        self.assertEqual(self.fake.visible()["Inbox/a.txt"]["fileInfo"]["src_last_modified_millis"], "1600000000000")
        self.assertEqual(self.fake.data("Inbox/tree/one.txt"), b"one")
        self.assertEqual(self.fake.data("Inbox/tree/sub/two.txt"), b"two")
        self.assertEqual(self.fake.data("Inbox/tree/empty/.bzEmpty"), b"")
        self.assertNotIn("Inbox/tree/link", self.fake.visible())
        self.assertIn("Skipped 1", answer.get("message", ""))
        self.assertTrue(os.path.exists(a), "copy keeps the source")
        self.assertEqual(self.listing("/Inbox")["a.txt"]["mtime"], 1_600_000_000)

    def test_into_the_bucket_root(self):
        a = self.write_local("a.txt", b"a")
        self.start()
        answer, _ = self.request(transfer("copy", [a], self.p("/"), True, False))
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("a.txt"), b"a")

    def test_into_an_entry_with_a_folder(self):
        a = self.write_local("a.txt", b"a")
        self.start(buckets=[self.entry(folder="backups/2026")])
        answer, _ = self.request(transfer("copy", [a], self.p("/"), True, False))
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("backups/2026/a.txt"), b"a")

    def test_large_file_goes_in_parts(self):
        data = os.urandom(5000) + b"tail"
        path = self.write_local("big.bin", data)
        self.fake.min_part = 1024
        self.start(FLEA_B2_PART_SIZE="1024")
        answer, _ = self.request(transfer("copy", [path], self.p("/"), True, False))
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("big.bin"), data)
        self.assertIn("large_file_sha1", self.fake.visible()["big.bin"]["fileInfo"])
        self.assertGreaterEqual(self.fake.calls.count("upload_part"), 5)

    def test_existing_name_refuses_before_any_byte(self):
        self.fake.put("a.txt", b"old")
        a = self.write_local("a.txt", b"new")
        b = self.write_local("b.txt", b"b")
        self.start()
        answer, _ = self.request(transfer("copy", [b, a], self.p("/"), True, False))
        self.assertEqual(answer["op"], "error")
        self.assertIn("already exists", answer["message"])
        self.assertEqual(self.fake.data("a.txt"), b"old")
        self.assertIsNone(self.fake.data("b.txt"))

    def test_move_removes_local_files(self):
        a = self.write_local("a.txt", b"a")
        tree = os.path.dirname(self.write_local("tree/x.txt", b"x"))
        self.start()
        answer, _ = self.request(transfer("move", [a, tree], self.p("/"), True, False))
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("tree/x.txt"), b"x")
        self.assertFalse(os.path.exists(a))
        self.assertFalse(os.path.exists(tree))

    def test_retries_upload_on_503_with_a_new_url(self):
        a = self.write_local("a.txt", b"a")
        self.fake.fail("upload", 503, count=2)
        self.start()
        answer, _ = self.request(transfer("copy", [a], self.p("/"), True, False))
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("a.txt"), b"a")
        self.assertGreaterEqual(self.fake.calls.count("b2_get_upload_url"), 3)

    def test_place_root_is_refused(self):
        a = self.write_local("a.txt", b"a")
        self.start()
        answer, _ = self.request(transfer("copy", [a], "/", True, False))
        self.assertEqual(answer["op"], "error")
        self.assertIn("Open one of the buckets", answer["message"])


class Download(PluginCase):
    def test_file_and_folder(self):
        self.fake.put("a.txt", b"alpha", mtime_ms=1_500_000_000_000)
        self.fake.put("Photos/lake.png", b"png")
        self.fake.put("Photos/2024/x.jpg", b"jpg")
        self.fake.put("Photos/empty/.bzEmpty")
        self.start()
        answer, _ = self.request(transfer("copy", [self.p("/a.txt"), self.p("/Photos")], self.local, False, True))
        self.assertEqual(answer["op"], "done", answer)
        with open(os.path.join(self.local, "a.txt"), "rb") as f:
            self.assertEqual(f.read(), b"alpha")
        self.assertEqual(int(os.path.getmtime(os.path.join(self.local, "a.txt"))), 1_500_000_000)
        with open(os.path.join(self.local, "Photos", "2024", "x.jpg"), "rb") as f:
            self.assertEqual(f.read(), b"jpg")
        self.assertTrue(os.path.isdir(os.path.join(self.local, "Photos", "empty")))
        self.assertFalse(os.path.exists(os.path.join(self.local, "Photos", "empty", ".bzEmpty")))
        self.assertEqual([n for n in os.listdir(self.local) if n.startswith(".")], [], "no partial file left")
        self.assertIsNotNone(self.fake.data("a.txt"), "copy keeps the source")

    def test_from_an_entry_with_a_folder(self):
        self.fake.put("backups/2026/a.txt", b"a")
        self.start(buckets=[self.entry(folder="/backups/2026/")])
        answer, _ = self.request(transfer("copy", [self.p("/a.txt")], self.local, False, True))
        self.assertEqual(answer["op"], "done", answer)
        self.assertTrue(os.path.exists(os.path.join(self.local, "a.txt")))

    def test_large_file_checksum_from_file_info(self):
        data = os.urandom(3000)
        path = self.write_local("up/big.bin", data)
        self.fake.min_part = 1024
        self.start(FLEA_B2_PART_SIZE="1024")
        self.assertEqual(self.request(transfer("copy", [path], self.p("/"), True, False))[0]["op"], "done")
        os.unlink(path)
        answer, _ = self.request(transfer("copy", [self.p("/big.bin")], self.local, False, True))
        self.assertEqual(answer["op"], "done", answer)
        with open(os.path.join(self.local, "big.bin"), "rb") as f:
            self.assertEqual(f.read(), data)

    def test_existing_local_name_refuses(self):
        self.fake.put("a.txt", b"remote")
        self.write_local("a.txt", b"local")
        self.start()
        answer, _ = self.request(transfer("copy", [self.p("/a.txt")], self.local, False, True))
        self.assertEqual(answer["op"], "error")
        self.assertIn("already exists", answer["message"])
        with open(os.path.join(self.local, "a.txt"), "rb") as f:
            self.assertEqual(f.read(), b"local")

    def test_move_hides_the_remote_file(self):
        self.fake.put("a.txt", b"a")
        self.start()
        answer, _ = self.request(transfer("move", [self.p("/a.txt")], self.local, False, True))
        self.assertEqual(answer["op"], "done", answer)
        self.assertTrue(os.path.exists(os.path.join(self.local, "a.txt")))
        self.assertTrue(self.fake.hidden("a.txt"))

    def test_vanished_source(self):
        self.start()
        answer, _ = self.request(transfer("copy", [self.p("/gone.txt")], self.local, False, True))
        self.assertEqual(answer["op"], "error")
        self.assertIn("no longer", answer["message"])

    def test_whole_bucket_entry_is_refused(self):
        self.fake.put("a.txt", b"a")
        self.start()
        answer, _ = self.request(transfer("move", [self.p("/")], self.local, False, True))
        self.assertEqual(answer["op"], "error")
        self.assertIn("whole bucket", answer["message"])
        self.assertFalse(self.fake.hidden("a.txt"))


class InsideBucket(PluginCase):
    def test_copy_file_and_folder(self):
        self.fake.put("a.txt", b"a", mtime_ms=1_000)
        self.fake.put("Photos/x.png", b"x")
        self.fake.put("Dest/.bzEmpty")
        self.start()
        answer, _ = self.request(transfer("copy", [self.p("/a.txt"), self.p("/Photos")], self.p("/Dest"), False, False))
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("Dest/a.txt"), b"a")
        self.assertEqual(self.fake.visible()["Dest/a.txt"]["fileInfo"]["src_last_modified_millis"], "1000")
        self.assertEqual(self.fake.data("Dest/Photos/x.png"), b"x")
        self.assertEqual(self.fake.data("a.txt"), b"a")

    def test_move_hides_sources(self):
        self.fake.put("Photos/x.png", b"x")
        self.fake.put("Dest/.bzEmpty")
        self.start()
        answer, _ = self.request(transfer("move", [self.p("/Photos")], self.p("/Dest"), False, False))
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("Dest/Photos/x.png"), b"x")
        self.assertTrue(self.fake.hidden("Photos/x.png"))

    def test_folder_into_itself_refuses(self):
        self.fake.put("Photos/sub/x.png", b"x")
        self.start()
        answer, _ = self.request(transfer("copy", [self.p("/Photos")], self.p("/Photos/sub"), False, False))
        self.assertEqual(answer["op"], "error")
        self.assertIn("inside itself", answer["message"])

    def test_same_folder_collides(self):
        self.fake.put("a.txt", b"a")
        self.start()
        answer, _ = self.request(transfer("copy", [self.p("/a.txt")], self.p("/"), False, False))
        self.assertEqual(answer["op"], "error")
        self.assertIn("already exists", answer["message"])

    def test_rename_file_and_folder(self):
        self.fake.put("a.txt", b"a")
        self.fake.put("Old/x.txt", b"x")
        self.fake.put("Old/deep/y.txt", b"y")
        self.start()
        answer, _ = self.request({"op": "rename", "paths": [self.p("/a.txt")], "path": self.p("/a.txt"),
                                  "to": "b.txt", "overwrite": False})
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("b.txt"), b"a")
        self.assertTrue(self.fake.hidden("a.txt"))
        answer, _ = self.request({"op": "rename", "paths": [self.p("/Old")], "path": self.p("/Old"), "to": "New",
                                  "overwrite": False})
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("New/deep/y.txt"), b"y")
        self.assertEqual(set(self.listing("/")), {"b.txt", "New"})

    def test_rename_onto_existing_refuses(self):
        self.fake.put("a.txt", b"a")
        self.fake.put("b.txt", b"b")
        self.start()
        answer, _ = self.request({"op": "rename", "paths": [self.p("/a.txt")], "path": self.p("/a.txt"),
                                  "to": "b.txt", "overwrite": False})
        self.assertEqual(answer["op"], "error")
        self.assertEqual(self.fake.data("b.txt"), b"b")

    def test_rename_whole_bucket_entry_is_refused(self):
        self.start()
        answer, _ = self.request({"op": "rename", "paths": [self.p("/")], "path": self.p("/"), "to": "Other",
                                  "overwrite": False})
        self.assertEqual(answer["op"], "error")
        self.assertIn("Manage buckets", answer["message"])


class BetweenBuckets(PluginCase):
    def test_copy_and_move_with_one_key_stay_server_side(self):
        self.fake.add_bucket("archive")
        self.fake.put("a.txt", b"a")
        self.fake.put("dir/b.txt", b"b")
        self.start(buckets=[self.entry(), self.entry(name="Archive", bucket="archive", folder="old")])
        answer, _ = self.request(transfer("copy", [self.p("/a.txt")], "/Archive", False, False))
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("old/a.txt", bucket="archive"), b"a")
        self.assertEqual(self.fake.data("a.txt"), b"a")
        answer, _ = self.request(transfer("move", [self.p("/dir")], "/Archive", False, False))
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("old/dir/b.txt", bucket="archive"), b"b")
        self.assertTrue(self.fake.hidden("dir/b.txt"))
        self.assertNotIn("download", self.fake.calls)

    def test_different_keys_are_refused(self):
        self.fake.add_bucket("other")
        self.fake.put("a.txt", b"a")
        self.start(buckets=[self.entry(), self.entry(name="Other", bucket="other", key_id="other-key")])
        answer, _ = self.request(transfer("copy", [self.p("/a.txt")], "/Other", False, False))
        self.assertEqual(answer["op"], "error")
        self.assertIn("different keys", answer["message"])

    def test_two_entries_on_one_bucket(self):
        self.fake.put("photos/a.jpg", b"a")
        self.fake.put("docs/b.pdf", b"b")
        self.start(buckets=[self.entry(name="Photos", folder="photos"), self.entry(name="Docs", folder="docs")])
        self.assertEqual(set(self.listing("/Photos", raw=True)), {"a.jpg"})
        self.assertEqual(set(self.listing("/Docs", raw=True)), {"b.pdf"})
        answer, _ = self.request(transfer("copy", ["/Photos/a.jpg"], "/Docs", False, False))
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(self.fake.data("docs/a.jpg"), b"a")


class Delete(PluginCase):
    def test_delete_hides_files_and_folders(self):
        self.fake.put("a.txt", b"a")
        self.fake.put("Photos/x.png", b"x")
        self.fake.put("Photos/sub/y.png", b"y")
        self.fake.put("Photos/empty/.bzEmpty")
        self.fake.put("keep.txt", b"k")
        self.start()
        answer, _ = self.request({"op": "delete", "paths": [self.p("/a.txt"), self.p("/Photos")]})
        self.assertEqual(answer["op"], "done", answer)
        self.assertEqual(set(self.listing("/")), {"keep.txt"})
        for name in ["a.txt", "Photos/x.png", "Photos/sub/y.png", "Photos/empty/.bzEmpty"]:
            self.assertTrue(self.fake.hidden(name), name)
        # Hiding keeps the old versions in B2.
        self.assertEqual(sum(1 for v in self.fake.versions if v["fileName"] == "a.txt"), 2)

    def test_marker_row_is_ignored(self):
        self.fake.put("a.txt", b"a")
        self.start()
        answer, _ = self.request({"op": "delete", "paths": [self.p("/" + MORE_MARKER), self.p("/a.txt")]})
        self.assertEqual(answer["op"], "done", answer)
        self.assertTrue(self.fake.hidden("a.txt"))

    def test_whole_bucket_entry_is_refused(self):
        self.fake.put("a.txt", b"a")
        self.start()
        answer, _ = self.request({"op": "delete", "paths": [self.p("/")]})
        self.assertEqual(answer["op"], "error")
        self.assertIn("whole bucket", answer["message"])
        self.assertFalse(self.fake.hidden("a.txt"))


class Background(PluginCase):
    wait_seconds = "0.5"

    def test_long_upload_continues_after_done(self):
        a = self.write_local("slow.txt", b"slow")
        self.fake.delays["upload"] = 2.0
        self.start()
        started = time.monotonic()
        answer, _ = self.request(transfer("copy", [a], self.p("/"), True, False))
        self.assertLess(time.monotonic() - started, 2.0)
        self.assertEqual(answer["op"], "done")
        self.assertIn("continues in the background", answer["message"])
        self.assertEqual(len(self.notifications()), 1)
        self.assertIn("continues", self.notifications()[0])
        for _ in range(100):
            if self.fake.data("slow.txt") is not None and len(self.notifications()) == 2:
                break
            time.sleep(0.1)
        self.assertEqual(self.fake.data("slow.txt"), b"slow")
        self.assertEqual(self.notifications()[1], "Uploaded 1 file.")
        jobs = os.path.join(self.env["XDG_RUNTIME_DIR"], "flea-b2", "jobs")
        for _ in range(20):
            if not os.listdir(jobs):
                break
            time.sleep(0.1)
        self.assertEqual(os.listdir(jobs), [])

    def test_background_failure_is_notified(self):
        self.fake.delays["download"] = 1.5
        self.start()
        answer, _ = self.request(transfer("copy", [self.p("/missing.txt")], self.local, False, True))
        # The check that the file exists happens before the slow download, so this one fails fast.
        self.assertEqual(answer["op"], "error")
        self.fake.put("slow.txt", b"x")
        self.fake.fail("download", 404, code="not_found")
        answer, _ = self.request(transfer("copy", [self.p("/slow.txt")], self.local, False, True))
        self.assertEqual(answer["op"], "done")
        for _ in range(60):
            if len(self.notifications()) == 2:
                break
            time.sleep(0.1)
        self.assertIn("failed", self.notifications()[-1])
        self.assertFalse(os.path.exists(os.path.join(self.local, "slow.txt")))

    def test_worker_survives_the_protocol_process(self):
        a = self.write_local("slow.txt", b"slow")
        self.fake.delays["upload"] = 1.5
        self.start()
        answer, _ = self.request(transfer("copy", [a], self.p("/"), True, False))
        self.assertEqual(answer["op"], "done")
        # Flea kills the plugin's process group when it goes idle; the worker is in its own session.
        os.killpg(self.process.pid, 9)
        self.process.wait()
        for _ in range(60):
            if self.fake.data("slow.txt") is not None:
                break
            time.sleep(0.1)
        self.assertEqual(self.fake.data("slow.txt"), b"slow")


if __name__ == "__main__":
    unittest.main()
