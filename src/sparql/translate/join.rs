//! Join (inner join) translator with batch SERVICE detection.

use std::collections::HashSet;

use spargebra::algebra::GraphPattern;
use spargebra::term::{NamedNodePattern, TermPattern};

use crate::sparql::federation;
use crate::sparql::sqlgen::{Ctx, Fragment};

pub(crate) fn translate_join(left: &GraphPattern, right: &GraphPattern, ctx: &mut Ctx) -> Fragment {
    // Batch SERVICE detection: combine two SERVICE clauses to same endpoint.
    if let (
        GraphPattern::Service {
            name: name_l,
            inner: inner_l,
            silent: silent_l,
        },
        GraphPattern::Service {
            name: name_r,
            inner: inner_r,
            silent: silent_r,
        },
    ) = (left, right)
        && let (NamedNodePattern::NamedNode(url_l), NamedNodePattern::NamedNode(url_r)) =
            (name_l, name_r)
    {
        let url_l_str = url_l.as_str();
        let url_r_str = url_r.as_str();
        if url_l_str == url_r_str {
            let vars_l = federation::collect_pattern_variables(inner_l);
            let vars_r = federation::collect_pattern_variables(inner_r);
            if vars_l.is_disjoint(&vars_r)
                && let Some(frag) = crate::sparql::translate::graph::translate_service_batched(
                    url_l_str,
                    inner_l,
                    inner_r,
                    *silent_l || *silent_r,
                    ctx,
                )
            {
                return frag;
            }
        }
    }
    // Standard join: merge fragments. Variables bound on both sides in every solution join on
    // plain `=`; the rest keep the null-safe form (FORK-JOIN-NULLSAFE-01).
    let certain: HashSet<String> = certain_vars(left)
        .intersection(&certain_vars(right))
        .cloned()
        .collect();
    let mut frag = crate::sparql::sqlgen::translate_pattern(left, ctx);
    let right_frag = crate::sparql::sqlgen::translate_pattern(right, ctx);
    frag.merge_certain(right_frag, &certain);
    frag
}

/// Variables that `p` binds in every solution (SPARQL "certainly bound"), conservatively: a
/// variable missing here may still always be bound, never the other way round.
pub(crate) fn certain_vars(p: &GraphPattern) -> HashSet<String> {
    fn term(t: &TermPattern, out: &mut HashSet<String>) {
        if let TermPattern::Variable(v) = t {
            out.insert(v.as_str().to_owned());
        }
    }
    match p {
        GraphPattern::Bgp { patterns } => {
            let mut out = HashSet::new();
            for tp in patterns {
                term(&tp.subject, &mut out);
                if let NamedNodePattern::Variable(v) = &tp.predicate {
                    out.insert(v.as_str().to_owned());
                }
                term(&tp.object, &mut out);
            }
            out
        }
        GraphPattern::Path {
            subject, object, ..
        } => {
            let mut out = HashSet::new();
            term(subject, &mut out);
            term(object, &mut out);
            out
        }
        GraphPattern::Join { left, right } | GraphPattern::Lateral { left, right } => {
            let mut out = certain_vars(left);
            out.extend(certain_vars(right));
            out
        }
        GraphPattern::LeftJoin { left, .. } | GraphPattern::Minus { left, .. } => {
            certain_vars(left)
        }
        GraphPattern::Union { left, right } => certain_vars(left)
            .intersection(&certain_vars(right))
            .cloned()
            .collect(),
        GraphPattern::Filter { inner, .. }
        | GraphPattern::Graph { inner, .. }
        | GraphPattern::Extend { inner, .. }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. } => certain_vars(inner),
        GraphPattern::Project { inner, variables } => {
            let keep: HashSet<&str> = variables.iter().map(|v| v.as_str()).collect();
            certain_vars(inner)
                .into_iter()
                .filter(|v| keep.contains(v.as_str()))
                .collect()
        }
        GraphPattern::Values {
            variables,
            bindings,
        } => variables
            .iter()
            .enumerate()
            .filter(|(i, _)| bindings.iter().all(|row| row.get(*i).is_some_and(Option::is_some)))
            .map(|(_, v)| v.as_str().to_owned())
            .collect(),
        // Group (aggregates may be unbound), Service (remote) and anything new: nothing assumed.
        _ => HashSet::new(),
    }
}

#[cfg(test)]
mod certain_tests {
    use super::*;

    fn algebra(q: &str) -> GraphPattern {
        match spargebra::SparqlParser::new().parse_query(q).expect("parse") {
            spargebra::Query::Select { pattern, .. } => pattern,
            _ => panic!("not a SELECT"),
        }
    }

    fn certain(q: &str) -> Vec<String> {
        let mut v: Vec<String> = certain_vars(&algebra(q)).into_iter().collect();
        v.sort();
        v
    }

    #[test]
    fn sub_select_star_is_certain_on_both_sides() {
        // the Overachiever whole-node read: the sub-SELECT's ?s and the star's ?s are always bound
        let q = "SELECT ?s ?p ?o WHERE { GRAPH <g:t> { { SELECT ?s WHERE { ?s <x:user> <u:1> ; <x:partOf> ?w } } ?s ?p ?o } }";
        assert_eq!(certain(q), ["o", "p", "s"]);
    }

    #[test]
    fn optional_and_union_and_undef_are_not_certain() {
        assert_eq!(certain("SELECT * WHERE { ?s <x:a> ?a OPTIONAL { ?s <x:b> ?b } }"), ["a", "s"]);
        assert_eq!(certain("SELECT * WHERE { { ?s <x:a> ?a } UNION { ?s <x:b> ?b } }"), ["s"]);
        assert_eq!(certain("SELECT * WHERE { VALUES (?x ?y) { (1 2) (3 UNDEF) } }"), ["x"]);
        assert_eq!(certain("SELECT ?s WHERE { ?s <x:a> ?a }"), ["s"]);
    }
}
