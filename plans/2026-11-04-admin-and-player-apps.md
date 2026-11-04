# Accepted request: separate admin and player interfaces

The user wants an admin panel for publishing and managing their modpack, while basic players only
run the lightweight background updater that checks for published updates.

Extend the existing Rust project with a separately named admin desktop application. Keep GitHub
credentials, Git and publishing operations out of the player workflow. Reuse the existing pack
publisher and distribution channel; do not return to Modrinth shared-instance uploads.

The admin panel must configure the live profile and pack checkout, show a read-only preview and
release notes, and expose an explicit Publish update action that uploads content, stages only
generated pack files, commits and pushes normally. Failed pushes must be retryable without creating
a duplicate commit. No force push, implicit staging of unrelated work, or automatic publication on
app startup. Include live bounded logs and durable full logs/operation state.

Support running the panel here against Mony over the existing SSH alias, so the profile and pack
stay on Mony. Use its existing GitHub CLI authentication; do not copy credentials to players. A local
admin configuration is also useful. SSH must retain normal host-key verification. Do not modify
production pack files while testing; test preview against the real source read-only and failures in
disposable fixtures or mocked command execution.

Keep the independent-profile copy and five-minute Windows tray updater from the preceding accepted
requests. The player installs/enrolls once, then future published pack updates are automatic while
Minecraft is closed. Existing configs remain copy-only-if-missing.

Implementing publication controls does not authorize this coding session to stage, commit, push,
tag or release the launcher changes or a new production pack. Request those exact operations only
after the result is reviewable and tested.
