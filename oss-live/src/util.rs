//! Shared helpers for the live engine: date math, repo-URL parsing, and the
//! tier/activity vocabulary mirrored from the stub so envelope rows keep a
//! consistent shape across engines.

/// Days between today and an RFC3339-ish timestamp (`YYYY-MM-DD...` prefix is
/// all we need). `None` when the date cannot be parsed.
pub fn days_since(date: &str) -> Option<u32> {
    let date = date.split_once('T').map(|(d, _)| d).unwrap_or(date);
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let then = days_from_civil(y, m, d);
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64
        / 86_400;
    (today - then).max(0).try_into().ok()
}

/// Howard Hinnant's days_from_civil algorithm (same as oss-facade npms.rs).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn star_tier(stars: u64) -> &'static str {
    match stars {
        s if s >= 10_000 => "high",
        s if s >= 500 => "medium",
        _ => "low",
    }
}

pub fn activity(days: u64) -> &'static str {
    match days {
        d if d <= 30 => "active",
        d if d <= 180 => "steady",
        _ => "stale",
    }
}

pub fn license_class(license: &str) -> &'static str {
    let l = license.to_ascii_uppercase();
    if l.starts_with("AGPL") {
        "AGPL"
    } else if l.contains("GPL") {
        "copyleft"
    } else if l.contains("MIT")
        || l.contains("APACHE")
        || l.contains("BSD")
        || l.contains("ISC")
        || l.contains("ZPL")
        || l.contains("0BSD")
    {
        "permissive"
    } else {
        "unknown"
    }
}

/// Split `owner/name` into its parts (`None` when not in that form).
pub fn split_repo(repo: &str) -> Option<(&str, &str)> {
    let (owner, name) = repo.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((owner, name))
}

/// `https://github.com/owner/repo` -> `owner/repo` (handles `.git` and
/// trailing slashes).
pub fn owner_name_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let without_git = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let after_host = without_git.split("://").nth(1).unwrap_or(without_git);
    let mut segs = after_host.split('/');
    let _host = segs.next()?;
    let owner = segs.next()?;
    let repo = segs.next()?;
    if owner.is_empty() || repo.is_empty() || segs.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// A probe like `/foo[Cc]lose/` is a regex probe; the slashes delimit.
pub fn strip_probe_delimiters(probe: &str) -> (&str, bool) {
    match probe.strip_prefix('/').and_then(|i| i.strip_suffix('/')) {
        Some(inner) if !inner.is_empty() => (inner, true),
        _ => (probe, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_since_today_is_zero() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let today = now / 86_400;
        let (y, m, d) = civil_from_days(today as i64);
        let date = format!("{y:04}-{m:02}-{d:02}T00:00:00Z");
        assert_eq!(days_since(&date), Some(0));
    }

    fn civil_from_days(z: i64) -> (i64, i64, i64) {
        let z = z + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        (y, m, d)
    }

    #[test]
    fn days_since_known_offset() {
        let epoch_days = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            / 86_400;
        assert_eq!(days_since("1970-01-01"), u32::try_from(epoch_days).ok());
        assert!(days_since("2020-01-01").unwrap() > 2000);
        assert_eq!(days_since("not-a-date"), None);
        // future dates clamp to zero rather than going negative
        assert_eq!(days_since("2999-01-01"), Some(0));
    }

    #[test]
    fn owner_name_from_url_variants() {
        assert_eq!(
            owner_name_from_url("https://github.com/expressjs/express"),
            Some("expressjs/express".to_string())
        );
        assert_eq!(
            owner_name_from_url("https://github.com/expressjs/express.git"),
            Some("expressjs/express".to_string())
        );
        assert_eq!(owner_name_from_url("git://github.com/a/b/"), Some("a/b".to_string()));
        assert_eq!(owner_name_from_url("https://github.com/a"), None);
        assert_eq!(owner_name_from_url("https://example.com/x/y/z"), None);
    }

    #[test]
    fn split_repo_and_probes() {
        assert_eq!(split_repo("a/b"), Some(("a", "b")));
        assert_eq!(split_repo("a/b/c"), None);
        assert_eq!(split_repo("nobody"), None);
        assert_eq!(strip_probe_delimiters("/foo[dD]/"), ("foo[dD]", true));
        assert_eq!(strip_probe_delimiters("spawn_worker"), ("spawn_worker", false));
        assert_eq!(strip_probe_delimiters("/"), ("/", false));
    }

    #[test]
    fn license_classes() {
        assert_eq!(license_class("MIT"), "permissive");
        assert_eq!(license_class("Apache-2.0"), "permissive");
        assert_eq!(license_class("GPL-3.0"), "copyleft");
        assert_eq!(license_class("AGPL-3.0"), "AGPL");
        assert_eq!(license_class(""), "unknown");
    }
}
