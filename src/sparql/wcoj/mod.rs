//! Worst-Case Optimal Join (WCOJ) optimiser for cyclic SPARQL patterns (v0.36.0).
//!
//! # Module layout (v0.114.0)
//!
//! | Sub-module | Contents |
//! |---|---|
//! | `executor`  | `WcojAnalysis`, planner hints, triangle benchmark |
//! | `trie`      | `SortedIterator`, `leapfrog_intersect`, `EdgeData` |
//! | `leapfrog`  | `CyclicBgpPattern`, `LftiBinding`, `execute_leapfrog_triejoin`, `try_leapfrog_select` |
//!
//! # GUC controls
//! - `pg_ripple.wcoj_enabled` (bool, default `true`) — master switch.
//! - `pg_ripple.wcoj_min_tables` (integer, default `3`) — minimum VP table joins.

use std::collections::{HashMap, HashSet};

pub mod executor;
pub mod leapfrog;
pub mod trie;

// Re-export so callers use `crate::sparql::wcoj::*` unchanged.
pub use executor::{analyse_bgp, apply_wcoj_hints, run_triangle_query, wcoj_session_preamble};
pub use leapfrog::{LftiBinding, try_leapfrog_select};

// ─── BGP variable extraction ──────────────────────────────────────────────────

/// Walk a `spargebra` `GraphPattern` and collect variable names per triple pattern.
pub fn extract_bgp_pattern_vars(pattern: &spargebra::algebra::GraphPattern) -> Vec<Vec<String>> {
    use spargebra::algebra::GraphPattern;
    use spargebra::term::{NamedNodePattern, TermPattern};

    let mut out: Vec<Vec<String>> = Vec::new();

    fn walk(p: &GraphPattern, out: &mut Vec<Vec<String>>) {
        match p {
            GraphPattern::Bgp { patterns } => {
                for tp in patterns {
                    let mut vars = Vec::new();
                    if let TermPattern::Variable(v) = &tp.subject {
                        vars.push(v.as_str().to_owned());
                    }
                    if let NamedNodePattern::Variable(v) = &tp.predicate {
                        vars.push(v.as_str().to_owned());
                    }
                    if let TermPattern::Variable(v) = &tp.object {
                        vars.push(v.as_str().to_owned());
                    }
                    if !vars.is_empty() {
                        out.push(vars);
                    }
                }
            }
            GraphPattern::Join { left, right }
            | GraphPattern::Union { left, right }
            | GraphPattern::LeftJoin { left, right, .. } => {
                walk(left, out);
                walk(right, out);
            }
            GraphPattern::Filter { inner, .. }
            | GraphPattern::Graph { inner, .. }
            | GraphPattern::Extend { inner, .. }
            | GraphPattern::OrderBy { inner, .. }
            | GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Reduced { inner }
            | GraphPattern::Slice { inner, .. }
            | GraphPattern::Group { inner, .. } => {
                walk(inner, out);
            }
            _ => {}
        }
    }

    walk(pattern, &mut out);
    out
}

// ─── Cycle detection ──────────────────────────────────────────────────────────

/// Detect whether a BGP is cyclic, i.e. not alpha-acyclic as a hypergraph whose hyperedges are the
/// variable sets of its triple patterns (GYO reduction: repeatedly drop variables that occur in
/// only one pattern and patterns whose variables are a subset of another pattern's; the BGP is
/// acyclic iff at most one pattern is left).
///
/// FORK-WCOJ-CYCLE-01: the previous check joined every pair of variables *within* a pattern into a
/// variable graph, so a single `?s ?p ?o` already formed a triangle and every star like
/// `?s a :T ; :user :u . ?s ?p ?o` was reported cyclic. That turned on the WCOJ planner hint
/// (`enable_hashjoin = off`, `join_collapse_limit = 1`) for plain star reads and forced nested
/// loops over the whole graph (Overachiever, Oct 2026: 27 ms → 150–1000 ms per history read).
pub fn detect_cyclic_bgp(pattern_vars: &[Vec<String>]) -> bool {
    if pattern_vars.len() < 3 {
        return false;
    }
    let mut edges: Vec<HashSet<String>> = pattern_vars
        .iter()
        .map(|vars| vars.iter().cloned().collect::<HashSet<String>>())
        .filter(|e| !e.is_empty())
        .collect();
    loop {
        let mut changed = false;
        let mut count: HashMap<String, usize> = HashMap::new();
        for e in &edges {
            for v in e {
                *count.entry(v.clone()).or_default() += 1;
            }
        }
        for e in edges.iter_mut() {
            let before = e.len();
            e.retain(|v| count.get(v).copied().unwrap_or(0) > 1);
            changed |= e.len() != before;
        }
        let mut i = 0;
        while i < edges.len() {
            let ear = edges[i].is_empty()
                || edges
                    .iter()
                    .enumerate()
                    .any(|(j, f)| j != i && edges[i].is_subset(f));
            if ear {
                edges.swap_remove(i);
                changed = true;
            } else {
                i += 1;
            }
        }
        if edges.len() <= 1 {
            return false;
        }
        if !changed {
            return true;
        }
    }
}

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use super::*;

    #[test]
    fn test_triangle_is_cyclic() {
        let p = vec![
            vec!["a".to_owned(), "b".to_owned()],
            vec!["b".to_owned(), "c".to_owned()],
            vec!["c".to_owned(), "a".to_owned()],
        ];
        assert!(detect_cyclic_bgp(&p));
    }

    #[test]
    fn test_star_is_acyclic() {
        let p = vec![
            vec!["root".to_owned(), "a".to_owned()],
            vec!["root".to_owned(), "b".to_owned()],
            vec!["root".to_owned(), "c".to_owned()],
        ];
        assert!(!detect_cyclic_bgp(&p));
    }

    #[test]
    fn test_chain_is_acyclic() {
        let p = vec![
            vec!["a".to_owned(), "b".to_owned()],
            vec!["b".to_owned(), "c".to_owned()],
        ];
        assert!(!detect_cyclic_bgp(&p));
    }

    #[test]
    fn test_square_is_cyclic() {
        let p = vec![
            vec!["a".to_owned(), "b".to_owned()],
            vec!["b".to_owned(), "c".to_owned()],
            vec!["c".to_owned(), "d".to_owned()],
            vec!["d".to_owned(), "a".to_owned()],
        ];
        assert!(detect_cyclic_bgp(&p));
    }

    #[test]
    fn test_single_not_cyclic() {
        assert!(!detect_cyclic_bgp(&[vec!["a".to_owned(), "b".to_owned()]]));
    }

    fn vs(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    /// FORK-WCOJ-CYCLE-01: three variables in one pattern are not a cycle.
    #[test]
    fn test_star_with_unbound_predicate_is_acyclic() {
        // ?s a :T ; :user :u ; :clientId ?c . ?s ?p ?o
        let p = vec![
            vs(&["s"]),
            vs(&["s"]),
            vs(&["s", "c"]),
            vs(&["s", "p", "o"]),
        ];
        assert!(!detect_cyclic_bgp(&p));
        // ?s :user :u ; :partOf ?w . ?s ?p ?o
        assert!(!detect_cyclic_bgp(&[
            vs(&["s"]),
            vs(&["s", "w"]),
            vs(&["s", "p", "o"])
        ]));
    }

    #[test]
    fn test_two_patterns_sharing_two_vars_is_acyclic() {
        // ?s :a ?o . ?s :b ?o . ?o :c ?x (alpha-acyclic: no WCOJ needed)
        assert!(!detect_cyclic_bgp(&[
            vs(&["s", "o"]),
            vs(&["s", "o"]),
            vs(&["o", "x"])
        ]));
    }

    #[test]
    fn test_triangle_with_star_arms_is_cyclic() {
        let p = vec![
            vs(&["a", "b"]),
            vs(&["b", "c"]),
            vs(&["c", "a"]),
            vs(&["a", "x"]),
            vs(&["b", "p", "o"]),
        ];
        assert!(detect_cyclic_bgp(&p));
    }
}
