"""Isolated Windows worker/tray smoke test. Never registers startup or uses player profiles.
Run: python scripts/test-background-windows.py target/release/jans-modpack-updater.exe
Evidence is kept in target/background-smoke-<time>/report.json.
"""
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time
import winreg

assert sys.platform == "win32"
exe = Path(sys.argv[1]).resolve(strict=True)
root = Path("target").resolve() / f"background-smoke-{time.time_ns()}"
profile = root / "ModrinthApp/profiles/Shared fixture"
home = root / "isolated-user-data"
profile.mkdir(parents=True)
home.mkdir()
db_path = profile.parent.parent / "app.db"
with sqlite3.connect(db_path) as db:
    db.executescript("""
    CREATE TABLE instances(id TEXT,name TEXT,path TEXT,applied_content_set_id TEXT);
    CREATE TABLE instance_content_sets(id TEXT,source_kind TEXT,game_version TEXT,loader TEXT,loader_version TEXT);
    CREATE TABLE instance_links(instance_id TEXT,link_kind TEXT);
    INSERT INTO instances VALUES('local:fixture','Shared fixture','Shared fixture','set');
    INSERT INTO instance_content_sets VALUES('set','shared_instance','26.3','fabric','0.20.1');
    INSERT INTO instance_links VALUES('local:fixture','shared_instance');
    """)
settings = {"enabled": True, "profile": str(profile), "instance_id": "local:fixture"}
settings_path = home / "background-profile.json"
settings_path.write_text(json.dumps(settings), encoding="utf-8")
env = dict(os.environ, JANS_LAUNCHER_HOME=str(home), JANS_MODRINTH_HOME=str(profile.parent.parent))
flags = subprocess.CREATE_NO_WINDOW

def startup_value():
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run") as key:
            return winreg.QueryValueEx(key, "JansModpackUpdater")
    except FileNotFoundError:
        return None

startup_before = startup_value()
db_before = hashlib.sha256(db_path.read_bytes()).hexdigest()
worker = subprocess.Popen([str(exe), "--background"], env=env, creationflags=flags)
try:
    deadline = time.monotonic() + 20
    status = {}
    while time.monotonic() < deadline:
        if worker.poll() is not None:
            raise AssertionError(f"Worker unexpectedly exited: {worker.returncode}")
        try:
            status = json.loads((home / "background-status.json").read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            pass
        if status.get("phase") == "error":
            break
        time.sleep(0.1)
    assert "local profile" in status.get("message", ""), status
    # The refused shared target guarantees this test cannot download the production pack.
    duplicate = subprocess.run([str(exe), "--background"], env=env, creationflags=flags, timeout=10)
    assert duplicate.returncode == 0
    metrics = json.loads(subprocess.check_output([
        "powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
        f"Get-Process -Id {worker.pid} | Select-Object WorkingSet64,PrivateMemorySize64,MainWindowHandle | ConvertTo-Json"
    ], creationflags=flags))
    assert metrics["MainWindowHandle"] == 0, metrics
    settings["enabled"] = False
    settings_path.write_text(json.dumps(settings), encoding="utf-8")
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        status = json.loads((home / "background-status.json").read_text())
        if status.get("phase") == "paused": break
        time.sleep(0.1)
    assert status["phase"] == "paused" and worker.poll() is None
finally:
    # Pause updates, then request explicit tray exit without killing the process.
    settings["enabled"] = False
    tmp = home / "disabled.json"
    tmp.write_text(json.dumps(settings), encoding="utf-8")
    os.replace(tmp, settings_path)
    (home / "background-exit").write_bytes(b"exit")
    worker.wait(timeout=15)

assert worker.returncode == 0
assert startup_value() == startup_before
assert hashlib.sha256(db_path.read_bytes()).hexdigest() == db_before
assert not (profile / "mods").exists()
waiting_home = root / "waiting-user-data"
waiting_home.mkdir()
waiting = subprocess.Popen([str(exe), "--background"], env=dict(env, JANS_LAUNCHER_HOME=str(waiting_home)), creationflags=flags)
try:
    deadline = time.monotonic() + 15
    waiting_status = {}
    while time.monotonic() < deadline:
        try: waiting_status = json.loads((waiting_home / "background-status.json").read_text())
        except (FileNotFoundError, json.JSONDecodeError): pass
        if waiting_status.get("phase") == "setup": break
        time.sleep(0.1)
    assert waiting.poll() is None and waiting_status.get("phase") == "setup", waiting_status
finally:
    (waiting_home / "background-exit").write_bytes(b"exit")
    waiting.wait(timeout=15)
assert waiting.returncode == 0 and startup_value() == startup_before
report = dict(passed=True, shared_target_refused=True, duplicate_exited=True,
              paused_resident=True, waiting_without_profile=True,
              disabled_cleanly=True, startup_unchanged=True, database_unchanged=True,
              metrics=metrics, status=status)
(root / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
print(json.dumps(dict(report=str(root / "report.json"), **report), indent=2))
