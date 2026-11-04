"""Deterministic admin regression suite. Git/publishing calls are fakes; no repositories mutated."""
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("backend", Path(__file__).resolve().parents[1] / "src/admin_backend.py")
b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b)

class Fake(b.Admin):
    head = "a" * 40
    remote = ""
    dirty = ""
    staged = ""
    fail_push = False
    fail_commit = False
    origin = "git@github.com:owner/pack.git"
    def __init__(self, options):
        self.commands = []
        self.published = 0
        self.events = []
        super().__init__(options, lambda line, **kwargs:self.events.append(json.loads(line)))
    def git(self, *args):
        self.commands.append(args)
        if args == ("rev-parse", "--show-toplevel"): return str(self.pack)
        if args == ("symbolic-ref", "--short", "HEAD"): return "main"
        if args == ("remote", "get-url", "origin"): return self.origin
        if args == ("rev-parse", "HEAD"): return self.head
        if args[0] == "status": return self.dirty
        if args[:3] == ("diff", "--quiet", "--cached"):
            if self.staged: raise RuntimeError("unrelated staged changes")
            return ""
        if args[:3] == ("diff", "--cached", "--name-only"): return self.staged
        if args[0] == "commit":
            if self.fail_commit: raise RuntimeError("commit hook failed")
            self.head = "b" * 40
            return ""
        if args[0] == "push":
            if self.fail_push: raise RuntimeError("network down")
            self.remote = self.head
            return ""
        if args[0] == "ls-remote": return self.remote + "\trefs/heads/main"
        return ""
    def publisher_run(self, preview):
        if not preview:
            self.published += 1
            (self.pack / "pack.json").write_text('{"version":"test"}')

class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.pack = root / "pack"
        self.profile = root / "profile"
        self.pack.mkdir(); self.profile.mkdir()
        (self.profile / "config").mkdir()
        (self.profile / "config/value.json").write_text("one")
        (self.pack / "publish.toml").write_text('minecraft="26.3"\ngithub_repo="owner/pack"\ninclude=["config/**"]\nexclude=["config/secret.json"]\n')
        publisher = root / "publisher"
        publisher.write_text("placeholder")
        self.env = patch.dict(os.environ, LOCALAPPDATA=str(root / "state"))
        self.env.start()
        self.admin = Fake(dict(pack=str(self.pack), profile=str(self.profile), publisher=str(publisher), version="test", notes="notes"))
    def tearDown(self):
        self.env.stop(); self.tmp.cleanup()
    def preview(self):
        self.admin.preview()
        self.admin.o["token"] = json.loads(self.admin.preview_path.read_text())["token"]
    def test_remote_normalization_and_credential_rejection(self):
        for url in ("git@github.com:owner/pack.git", "https://github.com/owner/pack", "ssh://git@github.com/owner/pack.git"):
            self.assertEqual(b.github_repo(url), "owner/pack")
        for url in ("https://token@github.com/owner/pack", "https://evil.test/owner/pack", "-oProxyCommand=evil"):
            with self.assertRaises(ValueError): b.github_repo(url)
    def test_only_generated_paths_can_be_staged(self):
        self.assertTrue(b.generated("overrides/config/a"))
        for path in ("publish.toml", "notes.md", "overrides2/a", "saves/a"):
            self.assertFalse(b.generated(path))
    def test_preview_is_readonly(self):
        self.preview()
        self.assertEqual(self.admin.published, 0)
        self.assertFalse((self.pack / "pack.json").exists())
        self.assertFalse(any(x[0] in ("add", "commit", "push") for x in self.admin.commands))
    def test_stale_preview_refuses_any_publication(self):
        self.preview(); (self.profile / "config/value.json").write_text("changed")
        with self.assertRaisesRegex(ValueError, "stale"): self.admin.publish()
        self.assertEqual(self.admin.published, 0)
    def test_unrelated_work_is_not_staged(self):
        self.admin.dirty = " M personal-notes.txt\0"
        with self.assertRaisesRegex(ValueError, "Unrelated"): self.admin.preview()
    def test_working_tree_spaces_are_preserved(self):
        self.admin.dirty = " M pack.json\0"
        self.preview()
    def test_push_failure_retries_only_the_existing_commit(self):
        self.preview(); self.admin.fail_push = True
        with self.assertRaisesRegex(RuntimeError, "network"): self.admin.publish()
        self.assertEqual(self.admin.record()["phase"], "committed")
        self.admin.fail_push = False; self.admin.push()
        self.assertEqual(sum(x[0] == "commit" for x in self.admin.commands), 1)
        self.assertEqual(self.admin.published, 1)
        self.assertEqual(self.admin.record()["phase"], "published")
        self.assertIn(("add", "--", "pack.json", "CHANGELOG.md", "overrides"), self.admin.commands)
        self.assertFalse(any("--force" in x or "-A" in x for x in self.admin.commands))
    def test_failed_hook_keeps_a_resumable_preparation(self):
        self.preview(); self.admin.fail_commit = True
        with self.assertRaisesRegex(RuntimeError, "hook"): self.admin.publish()
        self.assertEqual(self.admin.record()["phase"], "prepared")
        self.admin.fail_commit = False; self.admin.commit_prepared()
        self.assertEqual(self.admin.published, 1)
        self.assertEqual(self.admin.record()["phase"], "published")
    def test_changed_prepared_manifest_is_not_committed(self):
        self.preview(); self.admin.fail_commit = True
        with self.assertRaises(RuntimeError): self.admin.publish()
        (self.pack / "pack.json").write_text("changed")
        self.admin.fail_commit = False
        with self.assertRaisesRegex(ValueError, "manifest changed"): self.admin.commit_prepared()
    def test_origin_mismatch_is_refused(self):
        self.admin.origin = "git@github.com:other/pack.git"
        with self.assertRaisesRegex(ValueError, "Origin"): self.admin.preview()
    def test_prepared_override_must_match_the_manifest(self):
        self.preview()
        def changed_override(preview):
            folder = self.pack / "overrides/config"
            folder.mkdir(parents=True)
            (folder / "value.json").write_text("different bytes")
            (self.pack / "pack.json").write_text(json.dumps(dict(version="test", files=[dict(source="repo",url="overrides/config/value.json",sha512=hashlib.sha512(b"expected bytes").hexdigest())])))
        self.admin.publisher_run = changed_override
        with self.assertRaisesRegex(ValueError, "override changed"): self.admin.publish()
        self.assertFalse(any(x[0] == "commit" for x in self.admin.commands))
    def test_excluded_credentials_do_not_enter_fingerprint(self):
        before = self.admin.fingerprint()
        (self.profile / "config/secret.json").write_text("secret")
        self.assertEqual(before, self.admin.fingerprint())
    def test_changed_head_after_commit_refuses_automatic_retry(self):
        self.preview(); self.admin.fail_push = True
        with self.assertRaises(RuntimeError): self.admin.publish()
        self.admin.head = "c" * 40
        with self.assertRaisesRegex(ValueError, "matching committed"): self.admin.push()

if __name__ == "__main__": unittest.main(verbosity=2)
