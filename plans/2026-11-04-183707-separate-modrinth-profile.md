# Accepted: separate Modrinth profile for Jan's updater

The user accepted the following choices before implementation:

1. Create a separate Fabric 26.3 profile inside Modrinth for Jan's GitHub-managed modpack. The original shared profile remains untouched.
2. Copy everything from the old profile into the new profile, including mods, configs, worlds, screenshots and other personal files.
3. Keep the old profile until the user confirms that the new profile works. Do not delete it automatically.

The new profile must not inherit the old shared-instance/project update association. Modrinth continues to handle Microsoft sign-in, the game runtime and Play; Jan's updater handles GitHub-hosted pack updates. Preserve the user's established Once configuration policy.

Inspect the actual installed Modrinth integration before choosing how to register the new profile. Avoid unsupported edits to its database. Copy a consistent source after Minecraft is closed, and preserve both source data and recoverable destination state on failure.

No Git initialization, staging, commits, pushes, tags or releases are authorized by these profile choices. Obtain explicit permission for those operations once the implementation is reviewable.
