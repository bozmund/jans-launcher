# 0.4.0 pre-release validation — 2026-11-04

Implemented separate Rust admin and player updater interfaces. No production pack publication or
Git mutation was performed during this work.

- Windows `cargo test --locked --bins`: **41 passed** (20 launcher, 18 updater, 3 admin/shared).
- `python scripts/test-admin-backend.py`: **13 passed**, using fake Git/publisher commands.
  Covers read-only preview, stale source, unrelated changes, origin checks, failed commit hooks,
  failed push and commit reuse, changed receipts and manifest/override integrity.
- Release Windows binaries for admin and updater built successfully at version 0.4.0.
- Native admin CLI preview over verified SSH to Mony: exit 0, 857 source files, 136 Modrinth files,
  54 own assets and 667 overrides; no content changes since published pack 2026.11.04.
  Evidence: `target/admin-acceptance-20261104/native-preview-0.4.0.jsonl`.
- Mony pack repository was still clean at commit `7192865` after preview.
- Real Windows background-worker test passed in an isolated data folder: no visible window,
  shared target refused before fetching, duplicate worker exited, disabling stopped it normally,
  startup registry entry and fixture database unchanged. About 15.1 MB working set and 3.85 MB
  private memory in that idle/refused-target test; this is not a peak-download memory measurement.
  Evidence: `target/background-smoke-1793822151924442100/report.json`.
- The initial broader test run exposed a fixture-name collision on Windows: two tests received the
  same clock timestamp. The sync test fixture now includes an atomic counter; the full suite passed.

Review binaries and setup docs are copied to `../outputs/jans-admin-updater-0.4.0-preview/`.
The admin window was opened for the user to review, without screenshot/computer-use testing.

Not yet verified: real login startup, tray-menu interaction, full player onboarding, actual production
publication and real Git hook/push recovery, Linux build/runtime and Nix evaluation. CI is configured
to build/test all three binaries and run the deterministic Python recovery suite after authorized push.

Modrinth still performs native creation of the new local profile once; the updater then copies the
old player's files into it. It does not create/unlink profiles by writing Modrinth's private database.
Operational updater metadata is not copied, and unsafe linked paths are refused. User worlds and the
old shared profile's player data are retained. Existing configurations remain copy-only-if-missing.

Publication of the application requires explicit approval to stage, commit, push, tag and release
the prepared 0.4.0 changes. No Windows startup entry was enabled on the developer's real account.
