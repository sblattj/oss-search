use std::collections::HashSet;
use std::hash::Hash;

pub fn dcg(gains: &[u8], k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    gains
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, &g)| (2.0f64.powi(i32::from(g)) - 1.0) / ((i + 2) as f64).log2())
        .sum()
}

pub fn ndcg_at_k(gains: &[u8], k: usize) -> f64 {
    if k == 0 || gains.is_empty() {
        return 0.0;
    }
    let mut ideal: Vec<u8> = gains.to_vec();
    ideal.sort_unstable_by(|a, b| b.cmp(a));
    let idcg = dcg(&ideal, k);
    if idcg <= 0.0 {
        return 0.0;
    }
    dcg(gains, k) / idcg
}

pub fn mrr(relevant: &[bool]) -> f64 {
    relevant
        .iter()
        .position(|&r| r)
        .map_or(0.0, |i| 1.0 / (i + 1) as f64)
}

pub fn recall_at_k<T: Hash + Eq>(retrieved: &[T], relevant: &HashSet<T>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }
    if k == 0 {
        return 0.0;
    }
    let hits = retrieved
        .iter()
        .take(k)
        .filter(|d| relevant.contains(*d))
        .count();
    hits as f64 / relevant.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn dcg_hand_computed() {
        let gains = [3u8, 2, 3, 0, 1, 2];
        let terms: Vec<f64> = gains
            .iter()
            .enumerate()
            .map(|(i, &g)| {
                let gain = 2.0f64.powi(i32::from(g)) - 1.0;
                let disc = ((i + 2) as f64).log2();
                gain / disc
            })
            .collect();
        assert!(approx(terms[0], 7.0));
        assert!(approx(terms[1], 3.0 / 1.5849625007));
        assert!(approx(terms[2], 3.5));
        assert!(approx(terms[3], 0.0));
        assert!(approx(terms[4], 1.0 / 2.5849625007));
        assert!(approx(terms[5], 3.0 / 2.8073549221));
        let expected: f64 = terms.iter().sum();
        assert!(approx(expected, 13.848264));
        assert!(approx(dcg(&gains, 6), 13.848264));
        assert!(approx(dcg(&gains, 3), 7.0 + 1.8927893 + 3.5));
        assert_eq!(dcg(&gains, 0), 0.0);
        assert_eq!(dcg(&[], 5), 0.0);
    }

    #[test]
    fn ndcg_hand_computed_classic_permutation() {
        let gains = [3u8, 2, 3, 0, 1, 2];
        let dcg_v = 7.0 + 1.8927893 + 3.5 + 0.0 + 0.3868528 + 1.0686216;
        let idcg_v = 7.0 + 4.4165081 + 1.5 + 1.2920290 + 0.3868528 + 0.0;
        assert!(approx(dcg_v, 13.8482638));
        assert!(approx(idcg_v, 14.5953899));
        assert!(approx(ndcg_at_k(&gains, 6), dcg_v / idcg_v));
        assert!(approx(ndcg_at_k(&gains, 6), 0.948833));
    }

    #[test]
    fn ndcg_perfect_ordering_is_one() {
        assert!(approx(ndcg_at_k(&[3u8, 2, 1, 0], 4), 1.0));
        assert!(approx(ndcg_at_k(&[1u8, 0], 2), 1.0));
    }

    #[test]
    fn ndcg_relevant_demoted_to_rank_two() {
        let got = ndcg_at_k(&[0u8, 3], 2);
        assert!(approx(got, 1.0 / 1.5849625007));
        assert!(approx(got, 0.630930));
        let got = ndcg_at_k(&[0u8, 0, 3], 3);
        assert!(approx(got, 3.5 / 7.0));
        assert!(approx(got, 0.5));
    }

    #[test]
    fn ndcg_degenerate_inputs() {
        assert_eq!(ndcg_at_k(&[0u8, 0], 2), 0.0);
        assert_eq!(ndcg_at_k(&[3u8, 2], 0), 0.0);
        assert_eq!(ndcg_at_k(&[], 10), 0.0);
    }

    #[test]
    fn ndcg_k_beyond_length_clamps() {
        assert!(approx(ndcg_at_k(&[3u8, 1], 10), 1.0));
    }

    #[test]
    fn mrr_hand_computed() {
        assert!(approx(mrr(&[true]), 1.0));
        assert!(approx(mrr(&[false, true, false, true]), 0.5));
        assert!(approx(mrr(&[false, false, true]), 1.0 / 3.0));
        assert!(approx(mrr(&[false, false, false]), 0.0));
        assert!(approx(mrr(&[]), 0.0));
    }

    #[test]
    fn recall_hand_computed() {
        let mut relevant = HashSet::new();
        relevant.insert("a".to_string());
        relevant.insert("b".to_string());
        relevant.insert("c".to_string());
        let retrieved: Vec<String> = ["x", "a", "y", "b"].iter().map(|s| s.to_string()).collect();
        assert!(approx(recall_at_k(&retrieved, &relevant, 2), 1.0 / 3.0));
        assert!(approx(recall_at_k(&retrieved, &relevant, 4), 2.0 / 3.0));
        assert!(approx(recall_at_k(&retrieved, &relevant, 0), 0.0));
        assert!(approx(
            recall_at_k(&retrieved, &HashSet::<String>::new(), 4),
            1.0
        ));
    }
}
