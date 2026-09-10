use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendState {
    Ok,
    Degraded,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackendStatus {
    pub backend: String,
    pub status: BackendState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl BackendStatus {
    pub fn ok(backend: &str) -> Self {
        BackendStatus {
            backend: backend.to_string(),
            status: BackendState::Ok,
            detail: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Envelope<T> {
    pub results: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub has_more: bool,
    pub partial: bool,
    pub backend_status: Vec<BackendStatus>,
    pub warnings: Vec<String>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl<T: Serialize + Clone> Envelope<T> {
    pub fn serialized_len(&self) -> usize {
        serde_json::to_vec(self).map(|b| b.len()).unwrap_or(usize::MAX)
    }

    pub fn enforce_byte_budget(mut self, cap: usize) -> Self {
        if self.serialized_len() <= cap {
            return self;
        }
        let total = self.total.unwrap_or(self.results.len() as u64);
        let fits = |k: usize, note: &str| -> bool {
            let probe = Envelope {
                results: self.results[..k].to_vec(),
                total: Some(total),
                has_more: true,
                partial: self.partial,
                backend_status: self.backend_status.clone(),
                warnings: self.warnings.clone(),
                truncated: true,
                note: Some(note.to_string()),
                next_cursor: self.next_cursor.clone(),
            };
            probe.serialized_len() <= cap
        };
        let note_for = |k: usize| -> String {
            format!(
                "payload capped at {cap} bytes; showing {k} of {total} results — narrow the query, lower limit, or page with the cursor"
            )
        };
        let mut lo = 0usize;
        let mut hi = self.results.len();
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if fits(mid, &note_for(mid)) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        self.results.truncate(lo);
        self.truncated = true;
        self.has_more = true;
        self.note = Some(note_for(lo));
        if self.serialized_len() > cap {
            self.warnings.clear();
        }
        if self.serialized_len() > cap {
            self.note = None;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with_sizes(sizes: &[usize]) -> Envelope<serde_json::Value> {
        let results = sizes
            .iter()
            .map(|n| serde_json::json!({ "blob": "x".repeat(*n), "n": n }))
            .collect();
        Envelope {
            results,
            total: Some(sizes.len() as u64),
            has_more: false,
            partial: false,
            backend_status: vec![BackendStatus::ok("stub")],
            warnings: vec![],
            truncated: false,
            note: None,
            next_cursor: None,
        }
    }

    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    #[test]
    fn never_exceeds_cap_random_payloads() {
        let cap = 25_000;
        let mut rng = XorShift(0x9E3779B97F4A7C15);
        for trial in 0..200 {
            let n = (rng.next() % 120) as usize;
            let sizes: Vec<usize> = (0..n).map(|_| (rng.next() % 1200) as usize).collect();
            let original_len = env_with_sizes(&sizes).serialized_len();
            let env = env_with_sizes(&sizes).enforce_byte_budget(cap);
            let final_len = env.serialized_len();
            assert!(final_len <= cap, "trial {trial}: {final_len} > {cap}");
            if original_len <= cap {
                assert!(!env.truncated);
                assert_eq!(env.results.len(), sizes.len());
            } else {
                assert!(env.truncated, "trial {trial}: truncated flag not set");
                assert!(env.has_more, "trial {trial}: has_more not set");
                assert!(env.note.is_some(), "trial {trial}: note not set");
                assert!(env.results.len() < sizes.len());
            }
        }
    }

    #[test]
    fn single_oversized_result_fits_with_zero_results() {
        let env = env_with_sizes(&[80_000]).enforce_byte_budget(25_000);
        assert!(env.results.is_empty());
        assert!(env.truncated && env.has_more);
        assert!(env.serialized_len() <= 25_000);
    }

    #[test]
    fn keeps_max_results_that_fit() {
        let sizes: Vec<usize> = (0..40).map(|i| 1500 + i * 10).collect();
        let env = env_with_sizes(&sizes).enforce_byte_budget(25_000);
        let bigger = {
            let mut e2 = env.clone();
            e2.results.push(serde_json::json!({"blob": "x".repeat(1500)}));
            e2
        };
        assert!(bigger.serialized_len() > 25_000);
        assert!(env.serialized_len() <= 25_000);
    }

    #[test]
    fn small_envelope_untouched() {
        let env = env_with_sizes(&[10, 20]);
        let before = env.clone();
        let after = env.enforce_byte_budget(25_000);
        assert_eq!(before, after);
    }
}
