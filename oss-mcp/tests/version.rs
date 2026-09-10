//! Version gate: the workspace manifest version, the `oss-mcp` `VERSION`
//! constant, and the newest `CHANGELOG.md` heading must all agree. A release
//! that bumps one without the others fails here.

use std::fs;

/// Path to the workspace root, derived from this crate's manifest dir so the
/// test works from any checkout location.
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(path: &std::path::Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Extract `key = "value"` from the section `section` of a TOML document.
fn toml_section_string(doc: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in doc.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == format!("[{section}]") || line == format!("[{section}.package]");
            continue;
        }
        let Some(rest) = in_section
            .then(|| line.strip_prefix(key))
            .flatten()
            .map(|rest| rest.trim_start())
            .and_then(|rest| rest.strip_prefix('='))
            .map(|value| value.trim())
            .and_then(|value| value.strip_prefix('"'))
            .and_then(|value| value.strip_suffix('"'))
        else {
            continue;
        };
        return Some(rest.to_string());
    }
    None
}

/// Version declared in `[workspace.package]` of the root manifest.
fn workspace_version() -> String {
    let manifest = read(&workspace_root().join("Cargo.toml"));
    toml_section_string(&manifest, "workspace.package", "version")
        .unwrap_or_else(|| panic!("no [workspace.package] version in root Cargo.toml"))
}

/// Newest `## [x.y.z]` heading in CHANGELOG.md.
fn changelog_version() -> String {
    let changelog = read(&workspace_root().join("CHANGELOG.md"));
    changelog
        .lines()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("## [")?
                .split(']')
                .next()
                .map(|v| v.to_string())
        })
        .unwrap_or_else(|| panic!("no `## [x.y.z]` heading found in CHANGELOG.md"))
}

#[test]
fn workspace_version_matches_version_const() {
    let ws = workspace_version();
    assert_eq!(
        ws, oss_mcp::version::VERSION,
        "root Cargo.toml [workspace.package] version and oss-mcp VERSION const disagree"
    );
}

#[test]
fn changelog_heading_matches_workspace_version() {
    let ws = workspace_version();
    let changelog = changelog_version();
    assert_eq!(
        ws, changelog,
        "newest CHANGELOG.md heading ({changelog}) does not match workspace version ({ws})"
    );
}

#[test]
fn all_three_sources_read_0_2_0() {
    assert_eq!(workspace_version(), "0.2.0");
    assert_eq!(oss_mcp::version::VERSION, "0.2.0");
    assert_eq!(changelog_version(), "0.2.0");
}

#[test]
fn every_workspace_member_inherits_the_workspace_version() {
    let root = read(&workspace_root().join("Cargo.toml"));
    // Parse the `members = [ ... ]` array (may span multiple lines).
    let opener = "members = [";
    let start = root.find(opener).expect("no members array in root Cargo.toml") + opener.len();
    let end = start + root[start..].find(']').expect("unterminated members array");
    let members: Vec<String> = root[start..end]
        .split(',')
        .map(|entry| entry.trim().trim_matches('"').trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect();
    assert!(
        members.len() == 11,
        "expected 11 workspace members, parsed {members:?}"
    );
    for member in &members {
        let manifest = read(&workspace_root().join(member).join("Cargo.toml"));
        assert!(
            manifest.contains("version.workspace = true"),
            "{member}/Cargo.toml does not inherit the workspace version; \
             a standalone `version` line would drift from [workspace.package]"
        );
    }
}
