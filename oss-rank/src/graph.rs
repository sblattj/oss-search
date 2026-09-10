//! Dependency graph (`DepGraph`) with CSR storage and PageRank.
//!
//! Edges point **dependent → dependency** (the citation direction): mass
//! flows from dependents to dependencies, so PageRank ranks packages that
//! are *load-bearing* — being depended on by an important package counts
//! more than a flat dependent count (which weighs a throwaway scaffold the
//! same as a framework).
//!
//! The graph is stored as a uint32 CSR (offsets + sorted neighbor lists), so
//! power iteration is O(nodes + edges) per pass with deterministic loop
//! order — bit-identical scores across runs on the same input. Self-loops
//! (monorepo `@scope/*` packages that depend on siblings, or a package
//! nominally depending on itself) are dropped and duplicate edges deduped at
//! build time; the dependency graph is nearly acyclic, so power iteration
//! converges in a few dozen passes at `1e-10` tolerance.

use serde::{Deserialize, Serialize};

/// Directed dependency graph in CSR form. Node ids are caller-assigned
/// uint32 indices (e.g. a row number in the corpus package table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepGraph {
    node_count: u32,
    /// `offsets[u] .. offsets[u+1]` slices `edges` into u's dependencies,
    /// sorted ascending. Length `node_count + 1`.
    offsets: Vec<u32>,
    /// Flat, per-node-sorted out-edge targets (dependencies).
    edges: Vec<u32>,
    dropped_self_loops: usize,
    dropped_duplicates: usize,
}

impl DepGraph {
    /// Build a graph from `(dependent, dependency)` edges.
    ///
    /// Self-loops are dropped, duplicate edges deduped, and neighbor lists
    /// stored sorted — all deterministic regardless of input order. Edges
    /// with a node id `>= node_count` panic (caller owns the id space).
    pub fn from_edges<I>(node_count: u32, edges: I) -> Self
    where
        I: IntoIterator<Item = (u32, u32)>,
    {
        let raw: Vec<(u32, u32)> = edges.into_iter().collect();
        let mut list: Vec<(u32, u32)> = raw
            .iter()
            .copied()
            .filter(|&(u, v)| {
                assert!(u < node_count, "edge source {u} out of range");
                assert!(v < node_count, "edge target {v} out of range");
                u != v
            })
            .collect();
        let dropped_self_loops = raw.len() - list.len();
        list.sort_unstable();
        let before_dedup = list.len();
        list.dedup();
        let dropped_duplicates = before_dedup - list.len();

        let n = node_count as usize;
        let mut offsets = vec![0u32; n + 1];
        for &(u, _) in &list {
            offsets[u as usize + 1] += 1;
        }
        for i in 1..offsets.len() {
            offsets[i] += offsets[i - 1];
        }
        let mut edges = vec![0u32; list.len()];
        let mut cursor = offsets.clone();
        for &(u, v) in &list {
            let slot = &mut cursor[u as usize];
            edges[*slot as usize] = v;
            *slot += 1;
        }

        Self {
            node_count,
            offsets,
            edges,
            dropped_self_loops,
            dropped_duplicates,
        }
    }

    pub fn node_count(&self) -> u32 {
        self.node_count
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Self-loops dropped at build time (monorepo noise guard).
    pub fn dropped_self_loops(&self) -> usize {
        self.dropped_self_loops
    }

    /// Duplicate edges deduped at build time.
    pub fn dropped_duplicates(&self) -> usize {
        self.dropped_duplicates
    }

    /// Out-degree of `u` (number of distinct dependencies). Panics if
    /// `u >= node_count`.
    pub fn out_degree(&self, u: u32) -> u32 {
        self.neighbors(u).len() as u32
    }

    /// `u`'s distinct dependencies, sorted ascending. Panics if
    /// `u >= node_count`.
    pub fn neighbors(&self, u: u32) -> &[u32] {
        assert!(u < self.node_count, "node {u} out of range");
        let lo = self.offsets[u as usize] as usize;
        let hi = self.offsets[u as usize + 1] as usize;
        &self.edges[lo..hi]
    }

    /// Iterate all edges as `(dependent, dependency)` pairs in
    /// `(dependent, dependency)` sorted order.
    pub fn edges(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        (0..self.node_count).flat_map(|u| {
            let lo = self.offsets[u as usize] as usize;
            let hi = self.offsets[u as usize + 1] as usize;
            self.edges[lo..hi].iter().map(move |&v| (u, v))
        })
    }

    /// PageRank with uniform teleport (global importance). See
    /// [`DepGraph::personalized_page_rank`] for the full algorithm.
    pub fn page_rank(&self, damping: f64, tol: f64, max_iters: u32) -> PageRankResult {
        let uniform = vec![1.0 / self.node_count as f64; self.node_count as usize];
        self.page_rank_with_teleport(&uniform, damping, tol, max_iters)
    }

    /// Personalized PageRank: the teleport vector concentrates on `seeds`,
    /// so rank mass stays near the seed set — domain-scoped ranking
    /// (Haveliwala, topic-sensitive PageRank, WWW 2002).
    ///
    /// Seeds are deduped for determinism; an empty or all-invalid seed list
    /// falls back to uniform teleport. Personalized scores are only
    /// comparable within the same seed set — never mix across domains.
    pub fn personalized_page_rank(
        &self,
        seeds: &[u32],
        damping: f64,
        tol: f64,
        max_iters: u32,
    ) -> PageRankResult {
        let mut clean: Vec<u32> = seeds
            .iter()
            .copied()
            .filter(|&s| s < self.node_count)
            .collect();
        clean.sort_unstable();
        clean.dedup();
        if clean.is_empty() {
            return self.page_rank(damping, tol, max_iters);
        }
        let w = 1.0 / clean.len() as f64;
        let mut teleport = vec![0.0f64; self.node_count as usize];
        for &s in &clean {
            teleport[s as usize] = w;
        }
        self.page_rank_with_teleport(&teleport, damping, tol, max_iters)
    }

    /// Damped power iteration:
    ///
    /// ```text
    /// r' = (1−d)·v + d·(M·r + dangling_mass·v)
    /// ```
    ///
    /// where `v` is the teleport vector (sums to 1) and dangling mass (rank
    /// sitting on nodes with no out-edges — the norm in dependency graphs,
    /// since most packages have no dependents) is redistributed onto the
    /// teleport vector, the standard correction that keeps `Σr = 1`.
    /// Converges when the L1 change drops below `tol`; iteration order is
    /// fixed by the CSR layout, so results are deterministic.
    fn page_rank_with_teleport(
        &self,
        teleport: &[f64],
        damping: f64,
        tol: f64,
        max_iters: u32,
    ) -> PageRankResult {
        let n = self.node_count as usize;
        let mut r = teleport.to_vec();
        let mut next = vec![0.0f64; n];
        let mut converged = false;
        let mut iterations = 0u32;
        for it in 0..max_iters {
            iterations = it + 1;
            next.iter_mut().for_each(|x| *x = 0.0);
            for (ui, ru) in r.iter().enumerate() {
                let deg = self.out_degree(ui as u32);
                if deg == 0 {
                    continue;
                }
                let share = ru / deg as f64;
                for &v in self.neighbors(ui as u32) {
                    next[v as usize] += share;
                }
            }
            // Σ next == mass on non-dangling nodes, so 1 − Σ next is the
            // dangling mass (r sums to 1 from the teleport construction).
            let dangling = 1.0 - next.iter().sum::<f64>();
            let mut l1 = 0.0f64;
            for ((nv, tv), rv) in next.iter_mut().zip(teleport.iter()).zip(r.iter()) {
                *nv = (1.0 - damping) * tv + damping * (*nv + dangling * tv);
                l1 += (*nv - *rv).abs();
            }
            std::mem::swap(&mut r, &mut next);
            if l1 < tol {
                converged = true;
                break;
            }
        }
        PageRankResult {
            scores: r,
            iterations,
            converged,
        }
    }
}

/// PageRank output: one score per node (sums to 1), plus convergence
/// diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRankResult {
    pub scores: Vec<f64>,
    /// Iterations actually run (1 if the init was already stationary).
    pub iterations: u32,
    /// Whether the L1 residual dropped below tolerance before `max_iters`.
    pub converged: bool,
}

impl PageRankResult {
    /// Score of a node (0 if out of range — callers index sparse graphs).
    pub fn score(&self, node: u32) -> f64 {
        self.scores.get(node as usize).copied().unwrap_or(0.0)
    }

    /// Total score mass on a set of nodes (cluster totals, seed mass).
    pub fn total_on(&self, nodes: &[u32]) -> f64 {
        nodes.iter().map(|&n| self.score(n)).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn build_dedupes_edges_and_drops_self_loops_sorted() {
        let g = DepGraph::from_edges(
            4,
            [
                (0, 1),
                (0, 1), // duplicate
                (0, 0), // self-loop
                (0, 3),
                (2, 1),
            ],
        );
        assert_eq!(g.dropped_self_loops(), 1);
        assert_eq!(g.dropped_duplicates(), 1);
        assert_eq!(g.edge_count(), 3);
        assert_eq!(g.neighbors(0), &[1, 3]);
        assert_eq!(g.neighbors(1), &[] as &[u32]);
        assert_eq!(g.neighbors(2), &[1]);
        assert_eq!(g.edges().collect::<Vec<_>>(), vec![(0, 1), (0, 3), (2, 1)]);
    }

    #[test]
    fn build_is_input_order_independent() {
        let a = DepGraph::from_edges(4, [(0, 2), (0, 1), (3, 3), (1, 0)]);
        let b = DepGraph::from_edges(4, [(1, 0), (0, 1), (0, 2), (3, 3)]);
        assert_eq!(a.edges().collect::<Vec<_>>(), b.edges().collect::<Vec<_>>());
        let ra = a.page_rank(0.85, 1e-10, 100);
        let rb = b.page_rank(0.85, 1e-10, 100);
        assert_eq!(ra.scores, rb.scores);
    }

    #[test]
    fn pagerank_two_node_cycle_is_uniform() {
        // 0 ⇄ 1: the unique stationary distribution is (0.5, 0.5) for any
        // damping, exactly.
        let g = DepGraph::from_edges(2, [(0, 1), (1, 0)]);
        let pr = g.page_rank(0.85, 1e-12, 100);
        assert!(approx(pr.score(0), 0.5));
        assert!(approx(pr.score(1), 0.5));
        assert!(approx(pr.scores.iter().sum::<f64>(), 1.0));
    }

    #[test]
    fn pagerank_star_graph_matches_closed_form() {
        // Leaves 1,2,3 all depend on hub 0; hub is dangling.
        // Stationary conditions (d = 0.85, n = 4, k = 3 leaves):
        //   r_leaf = (1−d)/n + d·r_hub/n
        //   r_hub  = (1−d)/n + d·k·r_leaf + d·r_hub/n
        // ⇒ r_hub = ((1−d)/n)·(1 + d·k) / (1 − d/n − d²·k/n)
        let g = DepGraph::from_edges(4, [(1, 0), (2, 0), (3, 0)]);
        let (d, n, k) = (0.85f64, 4.0f64, 3.0f64);
        let pr = g.page_rank(d, 1e-12, 200);
        let hub = ((1.0 - d) / n * (1.0 + d * k)) / (1.0 - d / n - d * d * k / n);
        let leaf = (1.0 - d) / n + d * hub / n;
        assert!(approx(pr.score(0), hub));
        for &l in &[1u32, 2, 3] {
            assert!(approx(pr.score(l), leaf));
        }
        assert!(pr.converged);
        assert!(approx(pr.scores.iter().sum::<f64>(), 1.0));
    }

    #[test]
    fn pagerank_self_loop_edge_is_ignored_matches_closed_form() {
        // Edges (0,0) [dropped] and (0,1): node 0 depends on 1; 1 is
        // dangling. Without the self-loop, out-degree(0) = 1 and:
        //   r_0 = (1−d)/2 + d·r_1/2
        //   r_1 = (1−d)/2 + d·r_0 + d·r_1/2
        // ⇒ r_1 = (1−d)(1+d)/2 / (1 − d/2 − d²/2)
        let g = DepGraph::from_edges(2, [(0, 0), (0, 1)]);
        assert_eq!(g.dropped_self_loops(), 1);
        assert_eq!(g.out_degree(0), 1);
        let d = 0.85f64;
        let r1 = (1.0 - d) * (1.0 + d) / 2.0 / (1.0 - d / 2.0 - d * d / 2.0);
        let r0 = (1.0 - d) / 2.0 + d * r1 / 2.0;
        let pr = g.page_rank(d, 1e-12, 200);
        assert!(approx(pr.score(0), r0));
        assert!(approx(pr.score(1), r1));
    }

    #[test]
    fn pagerank_mass_flows_dependents_to_dependencies() {
        // Two 3-cycles A = {0,1,2}, B = {3,4,5} plus one bridge 0→3
        // ("A depends on B"). Importance flows A → B, so B's cluster total
        // must exceed A's and the bridged-to node must outrank the bridge
        // source, even though both clusters are otherwise symmetric.
        let g = DepGraph::from_edges(6, [(0, 1), (1, 2), (2, 0), (3, 4), (4, 5), (5, 3), (0, 3)]);
        let pr = g.page_rank(0.85, 1e-12, 500);
        let total_a = pr.total_on(&[0, 1, 2]);
        let total_b = pr.total_on(&[3, 4, 5]);
        assert!(total_b > total_a);
        assert!(total_b > 0.7, "cluster B should dominate, got {total_b}");
        assert!(pr.score(3) > pr.score(0));
        assert!(approx(total_a + total_b, 1.0));
    }

    #[test]
    fn pagerank_dangling_mass_keeps_sum_one() {
        // Chain 0→1→…→7: everything past 1 is reached once; nodes 7 and 1
        // (out-degree 0) are dangling. Dangling redistribution must keep the
        // distribution normalized and rank the sink above the sources.
        let edges: Vec<(u32, u32)> = (0..7u32).map(|i| (i, i + 1)).collect();
        let g = DepGraph::from_edges(8, edges);
        let pr = g.page_rank(0.85, 1e-12, 500);
        assert!(approx(pr.scores.iter().sum::<f64>(), 1.0));
        assert!(pr.score(7) > pr.score(0));
        assert!(pr.converged);
    }

    #[test]
    fn pagerank_converges_within_bounds_on_quasi_random_graph() {
        // Deterministic pseudo-random graph with cycles: converged well
        // under max_iters, matching the research expectation of a few dozen
        // passes on nearly-acyclic dependency-like graphs.
        let n = 200u32;
        let edges: Vec<(u32, u32)> = (0..n)
            .flat_map(|i| [(i, (7 * i + 3) % n), (i, (13 * i + 11) % n)])
            .collect();
        let g = DepGraph::from_edges(n, edges);
        let pr = g.page_rank(0.85, 1e-10, 500);
        assert!(pr.converged);
        assert!(pr.iterations <= 120, "took {} iterations", pr.iterations);
        assert!(approx(pr.scores.iter().sum::<f64>(), 1.0));
    }

    #[test]
    fn personalized_ppr_concentrates_on_seed_cluster() {
        // Same two-cluster bridge graph, but teleport concentrated on the
        // foundational (sink) cluster B. B's cycle has no out-edges to A,
        // so all mass — teleport plus everything crossing the bridge —
        // stays in B at stationarity.
        let g = DepGraph::from_edges(6, [(0, 1), (1, 2), (2, 0), (3, 4), (4, 5), (5, 3), (0, 3)]);
        let ppr = g.personalized_page_rank(&[3, 4, 5], 0.85, 1e-12, 500);
        let total_b = ppr.total_on(&[3, 4, 5]);
        assert!(total_b > 0.999, "B total {total_b}");
        assert!(approx(ppr.scores.iter().sum::<f64>(), 1.0));
        // Every seed-cluster node outranks every non-seed node.
        for &b in &[3u32, 4, 5] {
            for &a in &[0u32, 1, 2] {
                assert!(ppr.score(b) > ppr.score(a));
            }
        }
    }

    #[test]
    fn personalized_ppr_falls_back_to_uniform_on_empty_seeds() {
        let g = DepGraph::from_edges(3, [(0, 1), (1, 2)]);
        let uniform = g.page_rank(0.85, 1e-12, 200);
        let fallback = g.personalized_page_rank(&[], 0.85, 1e-12, 200);
        assert_eq!(uniform.scores, fallback.scores);
    }

    #[test]
    fn personalized_ppr_seed_dupes_and_out_of_range_are_cleaned() {
        let g = DepGraph::from_edges(3, [(0, 1), (1, 2)]);
        let a = g.personalized_page_rank(&[0, 0, 0, 99], 0.85, 1e-12, 200);
        let b = g.personalized_page_rank(&[0], 0.85, 1e-12, 200);
        assert_eq!(a.scores, b.scores);
        assert!(approx(a.scores.iter().sum::<f64>(), 1.0));
    }
}
