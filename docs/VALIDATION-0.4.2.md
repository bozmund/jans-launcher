# Player feedback fixes — 2026-11-04

User-reported symptoms: long setup without visible feedback; repeated Create clicks; closing the
window did not leave the app in the Windows notification area.

Implemented:
- Progress immediately beside the button: release/detection, source verification, byte-counted
  copying, saving and final verification, with elapsed time and an explicit native Install handoff.
- Process-wide shared setup lock plus persisted pending archive. Another click/restart reuses that
  archive, and a completed native import opens Modrinth rather than importing again.
- Both player executables understand the hidden worker mode. Closing starts the shared resident
  worker; active setup hides until completion instead of being discarded. The worker remains visible
  when updates are paused or it is waiting for native installation, and recognizes one completed
  marked independent profile. Explicit pause is respected. Windows startup uses a versioned executable.
- Branded tray icon, Explorer-startup retry and a retained tray-creation error rather than losing it
  to a generic stopped status. No player credentials are added to distribution.

Evidence:
- Windows `cargo test --locked --bins`: **52 passed** (27 launcher, 22 updater, 3 admin/shared).
- Regression verifies duplicate setup rejection, identical pending archive reuse (one archive only),
  observed progress stages, settings/world byte preservation and existing transaction safeguards.
- Actual hidden-worker smoke tests with **both player binaries** passed: duplicate worker exits,
  pause keeps the resident process alive, no-profile state waits visibly, explicit Exit ends cleanly,
  no visible app window and no real startup/fixture-database mutation. These runs used isolated
  JANS_LAUNCHER_HOME and JANS_MODRINTH_HOME directories.
- Debug smoke reports: `target/background-smoke-1793825634174136300/report.json` and
  `target/background-smoke-1793825644737409200/report.json`.
- Optimized release binaries also passed the same isolated smoke scenarios:
  `target/background-smoke-1793826087306026000/report.json` (updater) and
  `target/background-smoke-1793826092735276000/report.json` (launcher).

The actual X-button interaction, login startup and brother's full installer experience remain user
acceptance checks. Those are not claimed proven by the backend tests. No real player profile or
Windows startup registry entry was changed merely to test. Publication of version 0.4.2 was
explicitly approved on 2026-11-04; GitHub's Windows/Linux release gates remain required.

Separate mod repair candidates were prepared on Mony for the submitted game crash and explicit
Twilight Forest cloud exclusion. Both were installed with verified hashes and backups after
explicit approval on 2026-11-04; gameplay confirmation remains pending:
- TF: `/home/jan/Projects/TwilightForest-26.3/build/aurora-active-pass-20261104/candidate.json`.
- SC: `/home/jan/simple-clouds-fabric/build/twilight-dimension-20261104/candidate.json`.
