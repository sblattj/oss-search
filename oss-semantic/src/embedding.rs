//! Embedding abstraction.
//!
//! [`Embedder`] is the seam between the pipeline and any local inference
//! backend. Production builds use
//! [`FastEmbedder`](crate::fast_embedder::FastEmbedder) (cargo feature
//! `fastembed`, real local ONNX inference); tests use [`StubEmbedder`].

use crate::Result;

/// Embeds batches of texts into fixed-size dense vectors.
pub trait Embedder {
    /// Embed a batch of texts; result order matches input order.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// The dimensionality of every vector this embedder produces.
    fn dim(&self) -> usize;
}

/// Deterministic token-hashing embedder for tests.
///
/// Each alphanumeric token is hashed (FNV-1a) into one of `dim` buckets
/// and bucket counts are L2-normalized. The result is *semantically
/// meaningless* — two paraphrases of the same idea are unrelated, and
/// similarity only tracks token overlap. It exists so tests can exercise
/// the vector store and fusion logic with **no model download and no
/// network**; never use it in production.
#[derive(Debug, Clone)]
pub struct StubEmbedder {
    dim: usize,
}

impl StubEmbedder {
    /// Create a stub embedder with the given dimensionality.
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }
}

impl Default for StubEmbedder {
    fn default() -> Self {
        Self::new(256)
    }
}

/// FNV-1a hash (deterministic across platforms, releases, and runs —
/// unlike `std`'s `DefaultHasher`, which is only stable within a build).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl Embedder for StubEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut vec = vec![0.0f32; self.dim];
                for token in text.split(|c: char| !c.is_alphanumeric()) {
                    if token.is_empty() {
                        continue;
                    }
                    let bucket = (fnv1a(token.as_bytes()) % self.dim as u64) as usize;
                    vec[bucket] += 1.0;
                }
                let norm = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
                if norm > 0.0 {
                    for v in &mut vec {
                        *v /= norm;
                    }
                }
                vec
            })
            .collect())
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_is_deterministic() {
        let e = StubEmbedder::new(64);
        let a = e.embed(&["parse_json_from_file".into()]).unwrap();
        let b = e.embed(&["parse_json_from_file".into()]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn stub_respects_dim_and_normalizes() {
        let e = StubEmbedder::new(32);
        assert_eq!(e.dim(), 32);
        let v = e.embed(&["fn main() { }".into()]).unwrap()[0].clone();
        assert_eq!(v.len(), 32);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "expected unit norm, got {norm}");
    }

    #[test]
    fn stub_separates_disjoint_texts() {
        let e = StubEmbedder::new(128);
        let vs = e.embed(&["alpha".into(), "beta".into()]).unwrap();
        let dot: f32 = vs[0].iter().zip(&vs[1]).map(|(a, b)| a * b).sum();
        assert!(
            dot.abs() < 1e-6,
            "disjoint vocab should be orthogonal: {dot}"
        );
    }
}
