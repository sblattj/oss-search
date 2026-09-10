use crate::cas::Blake3Hash;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

pub const MIN_TOKENS_FOR_LSH: usize = 50;
pub const SHINGLE_WIDTH: usize = 5;
pub const NUM_PERM: usize = 128;
pub const LSH_BANDS: usize = 16;
const MERSENNE_PRIME_61: u64 = (1u64 << 61) - 1;
const PARAM_SEED: u64 = 0xC0FFEEC0DE5EED01;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum DupKind {
    Unique,
    Exact { of: Blake3Hash },
    Tokens { of: Blake3Hash },
    Near { of: Blake3Hash, jaccard: f64, estimated: f64 },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DupCounts {
    pub unique: u64,
    pub exact: u64,
    pub token: u64,
    pub near: u64,
}

pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

pub fn token_hash(tokens: &[String]) -> Blake3Hash {
    let mut hasher = blake3::Hasher::new();
    for token in tokens {
        hasher.update(token.as_bytes());
        hasher.update(&[0x1f]);
    }
    Blake3Hash(*hasher.finalize().as_bytes())
}

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

pub type ShingleSet = HashSet<u64>;

pub fn build_shingles(tokens: &[String]) -> ShingleSet {
    if tokens.len() < SHINGLE_WIDTH {
        if tokens.is_empty() {
            return ShingleSet::new();
        }
        let joined = tokens.join("\x1f");
        let mut set = ShingleSet::new();
        set.insert(fnv1a(joined.as_bytes()));
        return set;
    }
    let mut set = ShingleSet::with_capacity(tokens.len());
    for window in tokens.windows(SHINGLE_WIDTH) {
        let mut bytes = Vec::with_capacity(window.iter().map(|t| t.len()).sum::<usize>() + SHINGLE_WIDTH);
        for token in window {
            bytes.extend_from_slice(token.as_bytes());
            bytes.push(0x1f);
        }
        set.insert(fnv1a(&bytes));
    }
    set
}

fn minhash_params() -> (Vec<u64>, Vec<u64>) {
    let mut x = PARAM_SEED | 1;
    let mut a = Vec::with_capacity(NUM_PERM);
    let mut b = Vec::with_capacity(NUM_PERM);
    for _ in 0..NUM_PERM {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        a.push(x % (MERSENNE_PRIME_61 - 1) + 1);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        b.push(x % MERSENNE_PRIME_61);
    }
    (a, b)
}

pub fn signature(shingles: &ShingleSet) -> Vec<u64> {
    let (a, b) = minhash_params();
    let mut sig = vec![u64::MAX; NUM_PERM];
    for &shingle in shingles {
        for i in 0..NUM_PERM {
            let hashed = a[i]
                .wrapping_mul(shingle)
                .wrapping_add(b[i])
                % MERSENNE_PRIME_61;
            if hashed < sig[i] {
                sig[i] = hashed;
            }
        }
    }
    sig
}

pub fn estimated_jaccard(a: &[u64], b: &[u64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let equal = a.iter().zip(b).filter(|(x, y)| x == y).count();
    equal as f64 / a.len() as f64
}

pub fn exact_jaccard(a: &ShingleSet, b: &ShingleSet) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    if union == 0 {
        return 0.0;
    }
    inter as f64 / union as f64
}

struct LshIndex {
    rows: usize,
    buckets: HashMap<u64, Vec<Blake3Hash>>,
}

impl LshIndex {
    fn new() -> Self {
        Self {
            rows: NUM_PERM / LSH_BANDS,
            buckets: HashMap::new(),
        }
    }

    fn band_keys(sig: &[u64], rows: usize) -> Vec<u64> {
        (0..LSH_BANDS)
            .map(|band| {
                let mut bytes = Vec::with_capacity(8 + rows * 8);
                bytes.extend_from_slice(&(band as u64).to_le_bytes());
                for v in &sig[band * rows..(band + 1) * rows] {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
                fnv1a(&bytes)
            })
            .collect()
    }

    fn candidates(&self, sig: &[u64]) -> Vec<Blake3Hash> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for key in Self::band_keys(sig, self.rows) {
            if let Some(members) = self.buckets.get(&key) {
                for hash in members {
                    if seen.insert(*hash) {
                        out.push(*hash);
                    }
                }
            }
        }
        out
    }

    fn insert(&mut self, hash: Blake3Hash, sig: &[u64]) {
        for key in Self::band_keys(sig, self.rows) {
            self.buckets.entry(key).or_default().push(hash);
        }
    }
}

pub struct DedupEngine {
    pub threshold: f64,
    pub min_tokens: usize,
    seen: HashSet<Blake3Hash>,
    token_index: HashMap<Blake3Hash, Blake3Hash>,
    lsh: LshIndex,
    sketches: HashMap<Blake3Hash, (ShingleSet, Vec<u64>)>,
}

impl DedupEngine {
    pub fn new(threshold: f64) -> Self {
        Self {
            threshold,
            min_tokens: MIN_TOKENS_FOR_LSH,
            seen: HashSet::new(),
            token_index: HashMap::new(),
            lsh: LshIndex::new(),
            sketches: HashMap::new(),
        }
    }

    pub fn classify(&mut self, content: Blake3Hash, tokens: &[String]) -> DupKind {
        if self.seen.contains(&content) {
            return DupKind::Exact { of: content };
        }
        let thash = token_hash(tokens);
        if let Some(canonical) = self.token_index.get(&thash) {
            self.seen.insert(content);
            return DupKind::Tokens { of: *canonical };
        }
        let mut verdict = DupKind::Unique;
        let mut sketch = None;
        if tokens.len() >= self.min_tokens {
            let shingles = build_shingles(tokens);
            let sig = signature(&shingles);
            let mut best: Option<(Blake3Hash, f64, f64)> = None;
            for candidate in self.lsh.candidates(&sig) {
                let Some((other_shingles, other_sig)) = self.sketches.get(&candidate) else {
                    continue;
                };
                let est = estimated_jaccard(&sig, other_sig);
                let exact = exact_jaccard(&shingles, other_shingles);
                if exact >= self.threshold && best.is_none_or(|(_, _, be)| exact > be) {
                    best = Some((candidate, est, exact));
                }
            }
            if let Some((of, est, exact)) = best {
                verdict = DupKind::Near {
                    of,
                    jaccard: exact,
                    estimated: est,
                };
            }
            sketch = Some((shingles, sig));
        }
        self.seen.insert(content);
        self.token_index.entry(thash).or_insert(content);
        if let Some((shingles, sig)) = sketch {
            self.lsh.insert(content, &sig);
            self.sketches.insert(content, (shingles, sig));
        }
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens_of(text: &str) -> Vec<String> {
        tokenize(text)
    }

    #[test]
    fn tokenize_normalizes() {
        assert_eq!(
            tokens_of("let   Alpha_Beta = compute(42);"),
            vec!["let", "alpha", "beta", "compute", "42"]
        );
        assert_eq!(
            tokens_of("let\talpha=1;\nlet beta = 2;\n"),
            tokens_of("let alpha = 1; let beta = 2;")
        );
    }

    #[test]
    fn token_hash_stable_across_whitespace() {
        let a = tokenize("let alpha = 1; let beta = 2;");
        let b = tokenize("let\talpha=1;\nlet beta=2;");
        assert_eq!(token_hash(&a), token_hash(&b));
        assert_ne!(token_hash(&a), token_hash(&tokenize("let gamma = 3;")));
    }

    #[test]
    fn minhash_estimates_jaccard() {
        let base: Vec<String> = (0..400).map(|i| format!("token{i}")).collect();
        let mut variant = base.clone();
        for i in (0..400).step_by(70) {
            variant[i] = format!("changed{i}");
        }
        let a = build_shingles(&base);
        let b = build_shingles(&variant);
        let exact = exact_jaccard(&a, &b);
        let est = estimated_jaccard(&signature(&a), &signature(&b));
        assert!(exact >= 0.85, "exact {exact}");
        assert!((est - exact).abs() < 0.15, "est {est} vs exact {exact}");
    }

    #[test]
    fn near_dup_pair_flagged_and_verified() {
        let mut engine = DedupEngine::new(0.85);
        let base: Vec<String> = (0..300)
            .flat_map(|i| {
                [
                    format!("line{i}"),
                    format!("value{i}"),
                    format!("seed{i}"),
                    format!("arg{i}"),
                ]
            })
            .collect();
        let first = Blake3Hash::of(b"file-one");
        match engine.classify(first, &base) {
            DupKind::Unique => {}
            other => panic!("expected Unique, got {other:?}"),
        }
        let mut variant = base.clone();
        for i in (0..300).step_by(20) {
            variant[i * 4] = format!("modified{i}");
        }
        let second = Blake3Hash::of(b"file-two");
        match engine.classify(second, &variant) {
            DupKind::Near { of, jaccard, .. } => {
                assert_eq!(of, first);
                assert!(jaccard >= 0.85, "jaccard {jaccard}");
            }
            other => panic!("expected Near, got {other:?}"),
        }
    }

    #[test]
    fn dissimilar_files_not_flagged() {
        let mut engine = DedupEngine::new(0.85);
        let a: Vec<String> = (0..300).map(|i| format!("alpha{i} beta{i} gamma{i} delta{i} epsilon{i}")).collect();
        let b: Vec<String> = (0..300).map(|i| format!("zulu{i} yankee{i} xray{i} whiskey{i} victor{i}")).collect();
        assert!(matches!(engine.classify(Blake3Hash::of(b"a"), &a), DupKind::Unique));
        assert!(matches!(engine.classify(Blake3Hash::of(b"b"), &b), DupKind::Unique));
    }

    #[test]
    fn token_and_exact_tiers() {
        let mut engine = DedupEngine::new(0.85);
        let tokens = tokenize("let alpha = 1; let beta = 2;");
        let h1 = Blake3Hash::of(b"content-one");
        assert!(matches!(engine.classify(h1, &tokens), DupKind::Unique));
        assert!(matches!(
            engine.classify(Blake3Hash::of(b"content-two"), &tokens.clone()),
            DupKind::Tokens { of } if of == h1
        ));
        assert!(matches!(
            engine.classify(h1, &tokens),
            DupKind::Exact { of } if of == h1
        ));
    }
}
