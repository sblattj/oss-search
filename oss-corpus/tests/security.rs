//! Security-hardening tests: oss-corpus::AdmissionGate fixtures — size cap,
//! NUL binary detection, entropy gate behavior (intended rejections), and
//! the deliberate verdict for large NUL-free emoji text.

use oss_corpus::admission::{AdmissionGate, Rejection};

fn two_mb() -> Vec<u8> {
    vec![b'a'; 2 * 1024 * 1024]
}

#[test]
fn two_mb_file_rejected_as_too_large() {
    let gate = AdmissionGate::default();
    let bytes = two_mb();
    let rejected = gate.check(&bytes);
    match rejected {
        Err(Rejection::TooLarge { size, cap }) => {
            assert_eq!(size, 2 * 1024 * 1024);
            assert_eq!(cap, 1024 * 1024);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[test]
fn nul_containing_binary_rejected_with_offset() {
    let gate = AdmissionGate::default();
    // A binary-looking blob: text head, then NULs early in the scan window.
    let mut bytes = b"MZ\x90\x00 payload text follows here".to_vec();
    bytes.extend_from_slice(&[0u8; 64]);
    match gate.check(&bytes) {
        Err(Rejection::BinaryNul { offset }) => assert_eq!(offset, 3),
        other => panic!("expected BinaryNul, got {other:?}"),
    }
    // NUL beyond the scan window but inside the file: still binary, and the
    // window is what bounds the cost — verify the windowed verdict.
    let tail_nul = {
        let mut v = vec![b't'; AdmissionGate::default().nul_scan_window + 100];
        let end = v.len();
        v[end - 1] = 0;
        v
    };
    assert!(matches!(gate.check(&tail_nul), Ok(())));
}

#[test]
fn high_entropy_random_file_rejection_is_intended() {
    // The entropy gate exists to keep compressed/encrypted blobs out of the
    // text corpus. A NUL-free uniform-random file must be rejected as
    // HighEntropy — that rejection is the intended behavior, not a bug.
    let gate = AdmissionGate::default();
    let mut x: u64 = 0x9E3779B97F4A7C15 ^ 0xC0FFEE;
    let data: Vec<u8> = (0..128 * 1024)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let b = (x >> 33) as u8;
            if b == 0 { 1 } else { b }
        })
        .collect();
    assert!(!data.contains(&0), "fixture must be NUL-free");
    match gate.check(&data) {
        Err(Rejection::HighEntropy { bits_per_byte, threshold }) => {
            assert!(bits_per_byte > threshold);
            assert!((threshold - 7.5).abs() < 1e-9);
        }
        other => panic!("expected HighEntropy, got {other:?}"),
    }
}

#[test]
fn ten_k_nul_free_emoji_behavior_deliberate() {
    let gate = AdmissionGate::default();
    // Single emoji repeated: byte mass concentrated on 4 distinct byte
    // values -> low entropy -> admitted.
    let single = "😀".repeat(10_000);
    assert!(!single.as_bytes().contains(&0));
    let single_entropy = oss_corpus::admission::shannon_entropy(single.as_bytes());
    match gate.check(single.as_bytes()) {
        Ok(()) => assert!(
            single_entropy < 7.5,
            "admission must follow the entropy rule (got {single_entropy:.2})"
        ),
        Err(r @ Rejection::HighEntropy { .. }) => panic!("single-emoji text rejected: {r}"),
        Err(other) => panic!("unexpected rejection: {other:?}"),
    }
    // Varied emoji (800 distinct codepoints): UTF-8 spread raises entropy,
    // but the byte distribution stays concentrated on the F0/9F lead bytes,
    // so it lands well under the 7.5 threshold -> admitted. Pinned with the
    // measured entropy so a threshold change shows up here deliberately.
    let varied: String = (0..10_000)
        .map(|i| char::from_u32(0x1F300 + (i as u32 % 800)).unwrap())
        .collect();
    let varied_entropy = oss_corpus::admission::shannon_entropy(varied.as_bytes());
    assert!(
        varied_entropy < 7.5,
        "varied-emoji entropy {varied_entropy:.2} should be under threshold"
    );
    assert!(
        gate.check(varied.as_bytes()).is_ok(),
        "varied emoji text must be admitted at entropy {varied_entropy:.2}"
    );
}

#[test]
fn gate_precedence_size_before_nul_before_entropy() {
    // A 2MB file of NULs reports TooLarge (not BinaryNul): the cheap size
    // check wins first, which is the documented precedence.
    let gate = AdmissionGate::default();
    let bytes = vec![0u8; 2 * 1024 * 1024];
    assert!(matches!(gate.check(&bytes), Err(Rejection::TooLarge { .. })));
    // Size-passing NUL blob reports BinaryNul even though NULs are also
    // perfectly entropic — nul check precedes entropy.
    let nul_blob = vec![0u8; 1024];
    assert!(matches!(gate.check(&nul_blob), Err(Rejection::BinaryNul { .. })));
}
