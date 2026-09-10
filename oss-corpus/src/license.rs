use serde::Serialize;
use std::collections::HashSet;
use std::sync::OnceLock;

const LICENSE_TEXTS: &[(&str, &str)] = &[
    ("MIT", include_str!("license-data/MIT.txt")),
    ("ISC", include_str!("license-data/ISC.txt")),
    ("0BSD", include_str!("license-data/0BSD.txt")),
    ("Unlicense", include_str!("license-data/Unlicense.txt")),
    ("BSD-2-Clause", include_str!("license-data/BSD-2-Clause.txt")),
    ("BSD-3-Clause", include_str!("license-data/BSD-3-Clause.txt")),
    ("Apache-2.0", include_str!("license-data/Apache-2.0.txt")),
    ("MPL-2.0", include_str!("license-data/MPL-2.0.txt")),
    ("LGPL-2.1-only", include_str!("license-data/LGPL-2.1-only.txt")),
    ("GPL-2.0-only", include_str!("license-data/GPL-2.0-only.txt")),
    ("GPL-3.0-only", include_str!("license-data/GPL-3.0-only.txt")),
];

const FUZZY_THRESHOLD: f64 = 0.8;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LicenseMatch {
    pub spdx: String,
    pub confidence: f64,
}

fn normalized_signatures() -> &'static Vec<(&'static str, String)> {
    static SIGS: OnceLock<Vec<(&'static str, String)>> = OnceLock::new();
    SIGS.get_or_init(|| {
        LICENSE_TEXTS
            .iter()
            .map(|(id, text)| (*id, normalize(text)))
            .collect()
    })
}

pub fn normalize(text: &str) -> String {
    let mut lowered_lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("<<") || trimmed.ends_with(">>") {
            continue;
        }
        let lowered = trimmed.to_lowercase();
        if lowered.starts_with("copyright") {
            continue;
        }
        lowered_lines.push(lowered);
    }
    lowered_lines
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn prefix_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn trigrams(s: &str) -> HashSet<(char, char, char)> {
    let chars: Vec<char> = s.chars().collect();
    let mut set = HashSet::with_capacity(s.len());
    for w in chars.windows(3) {
        set.insert((w[0], w[1], w[2]));
    }
    set
}

fn dice_trigrams(a: &str, b: &str) -> f64 {
    let ta = trigrams(a);
    let tb = trigrams(b);
    if ta.is_empty() && tb.is_empty() {
        return 1.0;
    }
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count();
    (2 * inter) as f64 / (ta.len() + tb.len()) as f64
}

pub fn match_license_text(text: &str) -> Option<LicenseMatch> {
    let norm = normalize(text);
    if norm.len() < 64 {
        return None;
    }
    let sigs = normalized_signatures();
    for (id, sig) in sigs {
        if norm == *sig {
            return Some(LicenseMatch {
                spdx: (*id).to_string(),
                confidence: 1.0,
            });
        }
    }
    let query_prefix = prefix_chars(&norm, 200);
    let mut best: Option<(&str, f64, f64)> = None;
    for (id, sig) in sigs {
        let full_score = dice_trigrams(&norm, sig);
        let score = dice_trigrams(query_prefix, prefix_chars(sig, 200)).max(full_score);
        if score >= FUZZY_THRESHOLD
            && best.is_none_or(|(_, bs, bf)| (score, full_score) > (bs, bf))
        {
            best = Some((id, score, full_score));
        }
    }
    best.map(|(id, confidence, _)| LicenseMatch {
        spdx: id.to_string(),
        confidence,
    })
}

fn license_name_priority(name: &str) -> Option<u8> {
    let lowered = name.to_lowercase();
    let base = lowered.split('.').next().unwrap_or("");
    let stem = base.split(['-', '_']).next().unwrap_or("");
    match stem {
        "license" | "licence" => Some(0),
        "copying" => Some(1),
        "copyright" => Some(2),
        "notice" => Some(3),
        _ => None,
    }
}

pub fn is_license_file_name(name: &str) -> bool {
    license_name_priority(name).is_some()
}

pub fn detect_repo_license(root_files: &[(String, Vec<u8>)]) -> Option<(String, LicenseMatch)> {
    let mut candidates: Vec<(u8, &String, &Vec<u8>)> = root_files
        .iter()
        .filter_map(|(name, bytes)| {
            license_name_priority(name).map(|prio| (prio, name, bytes))
        })
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    for (_, name, bytes) in candidates {
        if let Ok(text) = std::str::from_utf8(bytes)
            && let Some(m) = match_license_text(text)
        {
            return Some((name.clone(), m));
        }
    }
    None
}

const SPDX_TAG: &str = "SPDX-License-Identifier:";
const SPDX_SCAN_WINDOW: usize = 8192;

pub fn scan_spdx_tag(bytes: &[u8]) -> Option<String> {
    let window = &bytes[..bytes.len().min(SPDX_SCAN_WINDOW)];
    let pos = memfind(window, SPDX_TAG.as_bytes())?;
    let start = pos + SPDX_TAG.len();
    let rest = &window[start..];
    let end = rest
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(rest.len());
    let line = String::from_utf8_lossy(&rest[..end]);
    let trimmed = line
        .trim()
        .trim_end_matches(['*', '/'])
        .trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    let valid = trimmed.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '+' | '(' | ')' | ',')
    });
    if valid {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn memfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIT_TEXT: &str = include_str!("license-data/MIT.txt");
    const BSD3_TEXT: &str = include_str!("license-data/BSD-3-Clause.txt");
    const GPL2_TEXT: &str = include_str!("license-data/GPL-2.0-only.txt");
    const APACHE_TEXT: &str = include_str!("license-data/Apache-2.0.txt");

    #[test]
    fn exact_mit_with_owner_substituted() {
        let text = MIT_TEXT.replace("<year> <copyright holders>", "2026 Stephen Blatt");
        let m = match_license_text(&text).unwrap();
        assert_eq!(m.spdx, "MIT");
        assert_eq!(m.confidence, 1.0);
    }

    #[test]
    fn fuzzy_mit_with_edited_wording() {
        let mut text = MIT_TEXT.replace("<year> <copyright holders>", "2026 Acme");
        text = text.replace("Permission is hereby granted", "Permission is hereby given");
        text.push_str("\nAdditional notice appended by the distributor.\n");
        let m = match_license_text(&text).unwrap();
        assert_eq!(m.spdx, "MIT");
        assert!(m.confidence >= 0.8, "confidence {}", m.confidence);
    }

    #[test]
    fn fuzzy_bsd3_with_appended_clause() {
        let mut text = BSD3_TEXT.replace("<copyright holder>", "Acme Inc.");
        text.push_str("\n4. You agree that this software is provided as-is in the state of California.\n");
        let m = match_license_text(&text).unwrap();
        assert_eq!(m.spdx, "BSD-3-Clause");
        assert!(m.confidence >= 0.8, "confidence {}", m.confidence);
    }

    #[test]
    fn exact_gpl2_and_apache() {
        assert_eq!(match_license_text(GPL2_TEXT).unwrap().spdx, "GPL-2.0-only");
        assert_eq!(match_license_text(APACHE_TEXT).unwrap().spdx, "Apache-2.0");
    }

    #[test]
    fn gpl2_with_modified_preamble_still_family_detected() {
        let text = GPL2_TEXT.replace(
            "free software",
            "libre software",
        );
        let m = match_license_text(&text).unwrap();
        assert_eq!(m.spdx, "GPL-2.0-only");
        assert!(m.confidence >= 0.8);
    }

    #[test]
    fn copyright_only_file_is_no_license() {
        let text = "Copyright (c) 2026 Acme Inc.\nAll rights reserved.\n";
        assert!(match_license_text(text).is_none());
    }

    #[test]
    fn spdx_tag_scan() {
        let file = b"// SPDX-License-Identifier: Apache-2.0\nfn main() {}\n";
        assert_eq!(scan_spdx_tag(file).as_deref(), Some("Apache-2.0"));
        let file = b"/* SPDX-License-Identifier: MIT OR Apache-2.0 */\npackage main\n";
        assert_eq!(scan_spdx_tag(file).as_deref(), Some("MIT OR Apache-2.0"));
        assert!(scan_spdx_tag(b"fn main() {}\n").is_none());
    }

    #[test]
    fn license_file_name_priority() {
        assert!(is_license_file_name("LICENSE"));
        assert!(is_license_file_name("LICENSE.md"));
        assert!(is_license_file_name("LICENCE.txt"));
        assert!(is_license_file_name("COPYING"));
        assert!(is_license_file_name("COPYING.LESSER"));
        assert!(is_license_file_name("LICENSE-MIT"));
        assert!(is_license_file_name("NOTICE"));
        assert!(!is_license_file_name("src/main.rs"));
        assert!(!is_license_file_name("README.md"));
    }
}
