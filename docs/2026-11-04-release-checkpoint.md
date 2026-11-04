# Release preparation checkpoint — 2026-11-04

## Confirmed user choice

Configuration remains `once`: copy a configuration only when it does not already exist. Never overwrite existing player configuration during pack updates.

## Completed

- Recoverable, instance-locked updates stage verified downloads before changing live files. Failed downloads preserve the installed pack and metadata; retries reuse verified cache.
- Replaced mods are retained in transaction backups. Fabric IDs identify older filename variants during migration from Modrinth. Unrelated personal mods and saves are preserved.
- Manifest paths and import paths are validated. Existing imported worlds are skipped as a whole rather than merged.
- Existing Modrinth profiles can receive updates without setting up the launcher's separate Microsoft application registration.
- Publishing scans only selected roots. Private Resourceful Config credentials and JEI per-world history are excluded.

## Verification

- Windows release built; standalone CLI HTTP acceptance passed: initial install, failed-download atomicity, cached retry, old-version replacement and preservation of Once configuration/personal mods/worlds. Evidence: `target/acceptance-20261104/results.json`.
- Linux pinned-Nix build passed 17 tests and release compilation. Managed job `0e0b2b9c82b14e8fb4224dd60ab72e0e` exited 0.
- Sanitized real-profile preview job `b4024b02ebac4f90a74587b1ad7f3ff9` exited 0. Manifest has 857 files: 562 Once configurations and 295 managed files. No saves, local-mods, JEI world history or Resourceful web credential file included.
- Windows preview SHA256: `91d95de2980594d6311d09acf9bf57c082b4245896fc86db66a5b998010839cb`.

## Not yet published

Explicit authorization for initial commits, repository creation, remote setup, pushes and releases was requested and remains pending. No Git state mutations or external publication were performed in this implementation turn.

Mony pack preview is `/home/jan/Projects/jans-modpack`; launcher build copy is `/home/jan/Projects/jans-launcher-20261104`. The preview used `--local-assets`; it must not be published unchanged. Real publication must upload the custom assets and regenerate hosted URLs, then publish the manifest.

GUI visual acceptance and end-user installation remain unverified. The Nix-built Linux executable is not a generic portable Linux artifact; the release workflow builds the distribution executable. Standalone Microsoft authentication still requires a configured application registration; Modrinth-profile update/play is the immediate supported path.
