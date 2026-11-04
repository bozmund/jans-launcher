"""On-demand admin helper, sent over authenticated SSH or run locally. JSON in, JSONL out.
Only explicit publish/retry actions mutate Git. No daemon or player credentials.
Requires Python 3.11+, git, gh and the existing jans-launcher publisher.
"""
import contextlib
import fnmatch
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import tomllib
from urllib.parse import unquote

ALLOWED = ("pack.json", "CHANGELOG.md", "overrides/")

def generated(path):
    return path in ALLOWED[:2] or path.startswith(ALLOWED[2])

def github_repo(url):
    match = re.fullmatch(r"(?:https://github\.com/|git@github\.com:|ssh://git@github\.com/)([\w.-]+/[\w.-]+?)(?:\.git)?/?", url.strip())
    if not match:
        raise ValueError("Origin must be the configured GitHub repository; credential-bearing/unknown URLs are refused")
    return match.group(1).lower()

class Admin:
    def __init__(self, options, emit=print):
        self.o = options
        self.pack = Path(options["pack"]).resolve(strict=True)
        self.profile = Path(options["profile"]).resolve(strict=True)
        self.publisher = Path(options["publisher"]).resolve(strict=True)
        self.cfg = tomllib.loads((self.pack / "publish.toml").read_text(encoding="utf-8"))
        if self.cfg["minecraft"] != "26.3":
            raise ValueError("Only the Fabric 26.3 pack is supported")
        base = Path(os.environ.get("LOCALAPPDATA", str(Path.home() / ".local/state"))) / "jans-pack-admin"
        self.directory = base / hashlib.sha256(str(self.pack).encode()).hexdigest()[:20]
        self.directory.mkdir(parents=True, exist_ok=True)
        self.record_path = self.directory / "publication.json"
        self.preview_path = self.directory / "preview.json"
        self.log_path = self.directory / (str(time.time_ns()) + ".log")
        self.emit = emit

    def event(self, kind, **values):
        item = {"type": kind, **values}
        with self.log_path.open("a", encoding="utf-8") as log:
            log.write(json.dumps(item) + "\n")
        self.emit(json.dumps(item), flush=True)

    def command(self, args, stream=False):
        if not stream:
            result = subprocess.run(args, cwd=self.pack, capture_output=True, text=True, encoding="utf-8", errors="replace")
            if result.returncode:
                # Never print command lines, which can contain credential-bearing remote URLs.
                raise RuntimeError(f"{Path(args[0]).name} failed (exit {result.returncode}): {result.stderr[-4000:]}")
            return result.stdout.rstrip("\r\n")
        child = subprocess.Popen(args, cwd=self.pack, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                 text=True, encoding="utf-8", errors="replace", bufsize=1)
        for line in child.stdout:
            self.event("log", text=line.rstrip()[:16000])
        code = child.wait()
        if code:
            raise RuntimeError(f"{Path(args[0]).name} failed (exit {code}); see the full log")
        return ""

    def git(self, *args):
        return self.command(["git", *args])

    def check_repository(self, allow_staged=False):
        top = Path(self.git("rev-parse", "--show-toplevel")).resolve()
        if top != self.pack:
            raise ValueError("Select the pack repository root, not a subfolder")
        branch = self.git("symbolic-ref", "--short", "HEAD")
        if branch != "main":
            raise ValueError("Publish from main; this app does not switch branches")
        if github_repo(self.git("remote", "get-url", "origin")) != self.cfg["github_repo"].lower():
            raise ValueError("Origin does not match github_repo in publish.toml")
        if allow_staged:
            for path in self.git("diff", "--cached", "--name-only", "-z").split("\0"):
                if path and not generated(path):
                    raise ValueError("Unrelated staged changes exist; refusing to commit them")
        else:
            self.git("diff", "--quiet", "--cached")
        dirty = self.git("status", "--porcelain=v1", "--untracked-files=all", "-z")
        # Reject renames/unusual paths rather than guessing a staging scope.
        for entry in dirty.split("\0"):
            if not entry:
                continue
            if len(entry) < 4 or entry[0] in "RC" or entry[1] in "RC" or not generated(entry[3:]):
                raise ValueError("Unrelated or ambiguous repository changes exist; review them outside the publisher first")
        return self.git("rev-parse", "HEAD")

    def fingerprint(self):
        digest = hashlib.sha256()
        digest.update((self.pack / "publish.toml").read_bytes())
        digest.update(json.dumps({k:self.o.get(k) for k in ("version", "notes", "profile", "publisher")}, sort_keys=True).encode())
        roots = {pattern.split("/")[0] for pattern in self.cfg["include"]}
        for root in sorted(roots):
            # Root glob patterns are not supported by this conservative source-stability guard.
            if any(c in root for c in "*?[") or root in ("", ".", ".."):
                raise ValueError("Admin preview needs explicit include root folders")
            base = self.profile / root
            if not base.exists():
                continue
            for path in sorted(base.rglob("*")):
                relative = path.relative_to(self.profile).as_posix()
                if any(fnmatch.fnmatchcase(relative, p) for p in self.cfg.get("exclude", [])):
                    continue
                if path.is_file():
                    digest.update(relative.encode())
                    with path.open("rb") as file:
                        digest.update(hashlib.file_digest(file, "sha256").digest())
        return digest.hexdigest()

    def snapshot(self):
        return {"head":self.check_repository(), "source":self.fingerprint(),
                "working":self.git("diff", "HEAD", "--", *ALLOWED[:2], "overrides"),
                "untracked":self.git("ls-files", "--others", "--exclude-standard", "-z")}

    @staticmethod
    def save(path, state):
        tmp = path.with_suffix(".tmp")
        with tmp.open("w", encoding="utf-8") as file:
            json.dump(state, file, indent=2)
            file.flush()
            os.fsync(file.fileno())
        os.replace(tmp, path)

    def record(self):
        return json.loads(self.record_path.read_text()) if self.record_path.exists() else {}

    def publisher_run(self, preview):
        args = [str(self.publisher), "publish", "--profile", str(self.profile), "--pack", str(self.pack)]
        if self.o.get("version"):
            args += ["--version", self.o["version"]]
        if self.o.get("notes"):
            args += ["--message", self.o["notes"]]
        if preview:
            args += ["--dry-run"]
        self.command(args, stream=True)

    def preview(self):
        pending = self.record()
        if pending.get("phase") not in (None, "published"):
            raise ValueError("An earlier publication needs recovery. Use Retry push if a commit was made; otherwise inspect its recorded state before trying again")
        before = self.snapshot()
        self.event("log", text="Scanning pack changes. Preview uploads and changes nothing in the pack.")
        self.publisher_run(True)
        if self.snapshot() != before:
            raise ValueError("The source or repository changed during preview. Run Preview again")
        token = hashlib.sha256(json.dumps(before, sort_keys=True).encode()).hexdigest()
        self.save(self.preview_path, dict(token=token, snapshot=before))
        self.event("result", ok=True, action="preview", token=token, log=str(self.log_path), message="Preview ready. Publish update uploads files, stages generated files, commits and pushes main.")

    def publish(self):
        preview = json.loads(self.preview_path.read_text())
        if self.o.get("token") != preview["token"] or self.snapshot() != preview["snapshot"]:
            raise ValueError("Preview is stale. Run Preview changes again before publishing")
        if self.record().get("phase") not in (None, "published"):
            raise ValueError("Previous publication is unfinished; do not create a duplicate commit")
        self.publisher_run(False)
        if self.fingerprint() != preview["snapshot"]["source"]:
            raise ValueError("Source changed during upload. Nothing committed; preview again")
        if self.check_repository() != preview["snapshot"]["head"]:
            raise ValueError("Repository HEAD changed during upload; nothing committed. Preview again")
        manifest = json.loads((self.pack / "pack.json").read_text())
        record = dict(phase="prepared", before=preview["snapshot"]["head"], version=manifest["version"], log=str(self.log_path), manifest_sha=hashlib.sha256((self.pack / "pack.json").read_bytes()).hexdigest())
        self.save(self.record_path, record)
        self.commit_prepared(record)

    def commit_prepared(self, record=None):
        record = record or self.record()
        if record.get("phase") != "prepared" or self.check_repository(allow_staged=True) != record.get("before"):
            raise ValueError("No unchanged prepared publication to resume. Inspect the saved record")
        if hashlib.sha256((self.pack / "pack.json").read_bytes()).hexdigest() != record.get("manifest_sha"):
            raise ValueError("Prepared manifest changed; refusing to commit different content")
        manifest = json.loads((self.pack / "pack.json").read_text())
        for entry in manifest.get("files", []):
            if entry.get("source") == "repo":
                relative = unquote(entry["url"])
                path = (self.pack / relative).resolve(strict=True)
                if not path.is_relative_to(self.pack / "overrides"):
                    raise ValueError("A repo file escapes overrides; refusing to publish")
                with path.open("rb") as file:
                    if hashlib.file_digest(file, "sha512").hexdigest() != entry["sha512"]:
                        raise ValueError("A prepared override changed; refusing to commit mismatched content")
        self.git("add", "--", "pack.json", "CHANGELOG.md", "overrides")
        self.check_repository(allow_staged=True)
        self.event("log", text="Committing only pack.json, CHANGELOG.md and overrides. Git hooks remain enabled.")
        try:
            self.git("commit", "-m", f"Pack {manifest['version']}")
        finally:
            head = self.git("rev-parse", "HEAD")
            if head != record["before"]:
                record.update(phase="committed", commit=head)
                self.save(self.record_path, record)
        self.push(record)

    def push(self, record=None):
        record = record or self.record()
        # Reconcile a crash after commit succeeded but before the receipt was saved.
        head = self.git("rev-parse", "HEAD")
        if record.get("phase") == "prepared" and head != record.get("before"):
            raise ValueError("Commit receipt is ambiguous. Inspect the saved record; no automatic additional commit or push")
        if record.get("phase") != "committed" or head != record.get("commit"):
            raise ValueError("No matching committed update to retry. Review the recorded state; nothing was committed again")
        self.check_repository()
        self.event("log", text="Pushing the recorded commit to origin/main (no force). A failure can be retried without another commit.")
        self.git("push", "origin", "HEAD:refs/heads/main")
        remote = self.git("ls-remote", "origin", "refs/heads/main").split()
        if not remote or remote[0] != head:
            raise RuntimeError("Push response did not confirm this commit. Retry push to verify; do not republish")
        record["phase"] = "published"
        self.save(self.record_path, record)
        self.event("result", ok=True, action="published", log=str(self.log_path), message=f"Published {record['version']}. Enrolled players will update at their next check after closing Minecraft.")

    @contextlib.contextmanager
    def lock(self):
        with (self.directory / "operation.lock").open("a+b") as file:
            if os.name == "nt":
                import msvcrt
                file.seek(0);file.write(b"0");file.flush();file.seek(0)
                msvcrt.locking(file.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            yield

def main():
    admin = None
    try:
        options = json.load(sys.stdin)
        admin = Admin(options)
        with admin.lock():
            action = options["action"]
            if action == "preview":
                admin.preview()
            elif action == "publish":
                admin.publish()
            elif action == "retry":
                admin.push()
            elif action == "resume":
                admin.commit_prepared()
            else:
                raise ValueError("Unknown admin action")
    except Exception as error:
        event = dict(ok=False, message=str(error))
        if admin:
            event.update(log=str(admin.log_path), recovery=admin.record())
            admin.event("result", **event)
        else:
            print(json.dumps(dict(type="result", **event)), flush=True)
        return 1
    return 0

if __name__ == "__main__":
    sys.exit(main())
