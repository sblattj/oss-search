//! Single source of truth for the user-facing `oss-mcp` version.
//!
//! The version gate (`tests/version.rs`) asserts that this constant, the
//! `[workspace.package]` version in the root `Cargo.toml`, and the newest
//! heading in `CHANGELOG.md` all agree, so a release cannot ship with a
//! stale constant or changelog.

/// Version of the `oss-mcp` binary, reported in MCP `serverInfo`.
pub const VERSION: &str = "0.2.0";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_semver() {
        let mut parts = VERSION.split('.');
        let (major, minor, patch) = (
            parts.next(),
            parts.next(),
            parts.next().and_then(|p| p.split(['-', '+']).next()),
        );
        assert!(parts.next().is_none(), "expected major.minor.patch, got {VERSION}");
        for part in [major, minor, patch] {
            let part = part.unwrap_or_default();
            assert!(
                !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()),
                "non-numeric version component in {VERSION}"
            );
        }
    }
}
