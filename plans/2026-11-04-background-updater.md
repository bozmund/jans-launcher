# Accepted request: automatic Windows pack updates

The user requests a minimal background application that starts with Windows, periodically checks for Jan's published modpack updates, and automatically updates the corresponding Modrinth profile. Friends should install it once rather than manually fetching each pack release.

Implement a user-owned Windows background process with a notification-area icon, startup registration for the current user, status and manual-check controls. Default checks run every five minutes. No administrator service is necessary. Closing the settings window must not stop an enabled background worker.

Enroll only the new independent, locally registered Fabric 26.3 profile. Preserve the old shared profile as required by the previously accepted separate-profile plan. Refuse automatic updates to shared or Modrinth-linked profiles. Revalidate profile identity and loader before writing.

Wait while Minecraft uses the selected profile, retry when it closes, and reuse verified downloads and transactional recovery. Never kill the game. Preserve worlds, settings, personal files and the existing config-only-if-missing policy. Failed checks keep an accessible error/status and retry without notification spam. Avoid repeatedly hashing the whole pack when the published revision has not changed.

Provide enable/disable controls, current state, last check and last successful update. Only one background worker may run. Use disposable fixtures for tests; do not alter existing player profiles to test. Do not enable startup on this computer merely as a test.

This request does not authorize Git staging, commits, pushes, tags or releases. Publish only after the concrete implementation is tested and the required Git operations are explicitly authorized.
