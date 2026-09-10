use serde::Serialize;
use std::fmt;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Serialize, Error)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Rejection {
    #[error("too_large: {size} bytes exceeds cap {cap}")]
    TooLarge { size: u64, cap: u64 },
    #[error("binary_nul: NUL byte at offset {offset}")]
    BinaryNul { offset: usize },
    #[error("high_entropy: {bits_per_byte:.2} bits/byte exceeds threshold {threshold:.2}")]
    HighEntropy { bits_per_byte: f64, threshold: f64 },
}

#[derive(Debug, Clone)]
pub struct AdmissionGate {
    pub max_bytes: u64,
    pub entropy_threshold: f64,
    pub nul_scan_window: usize,
}

impl Default for AdmissionGate {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024,
            entropy_threshold: 7.5,
            nul_scan_window: 8192,
        }
    }
}

impl AdmissionGate {
    pub fn check(&self, bytes: &[u8]) -> Result<(), Rejection> {
        if bytes.len() as u64 > self.max_bytes {
            return Err(Rejection::TooLarge {
                size: bytes.len() as u64,
                cap: self.max_bytes,
            });
        }
        let window = &bytes[..bytes.len().min(self.nul_scan_window)];
        if let Some(offset) = window.iter().position(|&b| b == 0) {
            return Err(Rejection::BinaryNul { offset });
        }
        let bits = shannon_entropy(bytes);
        if bits > self.entropy_threshold {
            return Err(Rejection::HighEntropy {
                bits_per_byte: bits,
                threshold: self.entropy_threshold,
            });
        }
        Ok(())
    }
}

pub fn shannon_entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let total = bytes.len() as f64;
    let mut entropy = 0.0;
    for &c in counts.iter() {
        if c > 0 {
            let p = c as f64 / total;
            entropy -= p * p.log2();
        }
    }
    entropy
}

impl fmt::Display for AdmissionGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AdmissionGate(max_bytes={}, entropy_threshold={:.2})",
            self.max_bytes, self.entropy_threshold
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_of_known_inputs() {
        assert_eq!(shannon_entropy(b"aaaa"), 0.0);
        assert!((shannon_entropy(b"ab") - 1.0).abs() < 1e-9);
        assert!(shannon_entropy(b"abcdef") > 2.0);
    }

    #[test]
    fn rejects_oversized() {
        let gate = AdmissionGate {
            max_bytes: 16,
            ..Default::default()
        };
        assert_eq!(
            gate.check(&[b'x'; 17]),
            Err(Rejection::TooLarge { size: 17, cap: 16 })
        );
    }

    #[test]
    fn rejects_nul_binary() {
        let gate = AdmissionGate::default();
        let data = b"MZ payload text\x00\x00\x00more text data".repeat(8);
        match gate.check(&data) {
            Err(Rejection::BinaryNul { offset }) => assert_eq!(offset, 15),
            other => panic!("expected BinaryNul, got {other:?}"),
        }
    }

    #[test]
    fn rejects_high_entropy_without_nul() {
        let gate = AdmissionGate::default();
        let mut x: u64 = 0x9E3779B97F4A7C15;
        let data: Vec<u8> = (0..128 * 1024)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let b = (x >> 33) as u8;
                if b == 0 {
                    1
                } else {
                    b
                }
            })
            .collect();
        match gate.check(&data) {
            Err(Rejection::HighEntropy { bits_per_byte, .. }) => {
                assert!(bits_per_byte > 7.5, "entropy {bits_per_byte}");
            }
            other => panic!("expected HighEntropy, got {other:?}"),
        }
    }

    #[test]
    fn accepts_plain_source() {
        let gate = AdmissionGate::default();
        assert!(gate
            .check(b"fn main() { println!(\"hello\"); }\n".repeat(100).as_slice())
            .is_ok());
    }
}
