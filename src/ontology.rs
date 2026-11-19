//! The index a rewrite reads: for a property or a class, every spelling a
//! store may hold that the ontology says satisfies it.
//!
//! It is built from the ontology alone, never from what a store holds. An
//! index that depended on the data would go stale the first time a new
//! spelling was written, and the result would be an empty answer with no
//! error.
//!
//! The rules, and there are only these:
//!
//!   - a property is satisfied by itself, by anything declared
//!     `rdfs:subPropertyOf` it, and by anything `owl:equivalentProperty` to
//!     it or to one of those, in either direction;
//!   - a class is satisfied by itself, by anything declared
//!     `rdfs:subClassOf` it, and by anything `owl:equivalentClass` to it or
//!     to one of those, in either direction. Only named classes count; a
//!     restriction or any other unnamed class is ignored.
//!
//! Left out on purpose: `rdfs:domain`, `rdfs:range`, `owl:inverseOf`,
//! `owl:sameAs` and every "there exists some" rule. So this is the hierarchy
//! part of OWL 2 QL, not the whole profile.
//!
//! ABSTRACT NAMES. A stack may keep a layer of shared names that are never
//! written into stored data (in Subtexture, everything under
//! `https://repolex.ai/ontology/subtexture/`). The host names those prefixes
//! and they are dropped from every answer, so a shared name that one store
//! property bridges to becomes a plain swap of one name for another. The host
//! must make sure no store ever holds a statement with such a name, because
//! if one does, dropping the name hides real data.

use oxrdf::{NamedNode, NamedNodeRef, NamedOrBlankNodeRef, TermRef, TripleRef};
use std::collections::{BTreeSet, HashMap};

pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SUB_PROPERTY: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
const SUB_CLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const EQUIVALENT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#equivalentProperty";
const EQUIVALENT_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";

/// What to ask for in place of one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Targets<'a> {
    /// Ask for the name as written.
    Same,
    /// Ask for this one name instead.
    One(&'a NamedNode),
    /// Ask for each of these, in this order. Always two or more.
    Many(&'a [NamedNode]),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    properties: HashMap<String, Vec<NamedNode>>,
    classes: HashMap<String, Vec<NamedNode>>,
}

impl Index {
    /// Build the index from an ontology's statements. Statements that are
    /// not one of the four kinds above are ignored, so a whole ontology file
    /// can be passed. `abstract_prefixes` are the address prefixes of names
    /// that are never stored (may be empty).
    pub fn build<'a>(triples: impl IntoIterator<Item = TripleRef<'a>>, abstract_prefixes: &[&str]) -> Self {
        // An edge points from a name to a name that satisfies it.
        let mut properties: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut classes: HashMap<String, BTreeSet<String>> = HashMap::new();
        for t in triples {
            let (NamedOrBlankNodeRef::NamedNode(s), TermRef::NamedNode(o)) = (t.subject, t.object) else { continue };
            let (edges, both_ways) = match t.predicate.as_str() {
                SUB_PROPERTY => (&mut properties, false),
                EQUIVALENT_PROPERTY => (&mut properties, true),
                SUB_CLASS => (&mut classes, false),
                EQUIVALENT_CLASS => (&mut classes, true),
                _ => continue,
            };
            edges.entry(o.as_str().to_string()).or_default().insert(s.as_str().to_string());
            edges.entry(s.as_str().to_string()).or_default();
            if both_ways {
                edges.entry(s.as_str().to_string()).or_default().insert(o.as_str().to_string());
            }
        }
        Index { properties: closure(&properties, abstract_prefixes), classes: closure(&classes, abstract_prefixes) }
    }

    /// Build the index from Turtle the host holds in memory, one text per
    /// ontology file.
    #[cfg(feature = "turtle")]
    pub fn from_turtle(texts: &[&str], abstract_prefixes: &[&str]) -> crate::Result<Self> {
        let mut triples = Vec::new();
        for text in texts {
            for triple in oxttl::TurtleParser::new().for_slice(text.as_bytes()) {
                triples.push(triple.map_err(|e| format!("the ontology is not readable Turtle: {e}"))?);
            }
        }
        Ok(Self::build(triples.iter().map(oxrdf::Triple::as_ref), abstract_prefixes))
    }

    pub fn property(&self, name: NamedNodeRef<'_>) -> Targets<'_> {
        if name.as_str() == RDF_TYPE {
            return Targets::Same;
        }
        targets(self.properties.get(name.as_str()))
    }

    pub fn class(&self, name: NamedNodeRef<'_>) -> Targets<'_> {
        targets(self.classes.get(name.as_str()))
    }

    /// How many properties and classes are asked for under other names.
    pub fn len(&self) -> (usize, usize) {
        (self.properties.len(), self.classes.len())
    }

    pub fn is_empty(&self) -> bool {
        self.properties.is_empty() && self.classes.is_empty()
    }
}

fn targets(found: Option<&Vec<NamedNode>>) -> Targets<'_> {
    match found.map(Vec::as_slice) {
        None | Some([]) => Targets::Same,
        Some([one]) => Targets::One(one),
        Some(many) => Targets::Many(many),
    }
}

/// For every name, everything reachable from it, itself included, without
/// the abstract names, in address order. A name whose answer is only itself,
/// or nothing at all, is left out: it is asked for as written.
fn closure(edges: &HashMap<String, BTreeSet<String>>, abstract_prefixes: &[&str]) -> HashMap<String, Vec<NamedNode>> {
    let mut out = HashMap::new();
    for start in edges.keys() {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut todo = vec![start.as_str()];
        while let Some(name) = todo.pop() {
            if seen.insert(name) {
                if let Some(next) = edges.get(name) {
                    todo.extend(next.iter().map(String::as_str));
                }
            }
        }
        let stored: Vec<&str> = seen.into_iter().filter(|n| !abstract_prefixes.iter().any(|p| n.starts_with(p))).collect();
        if stored.is_empty() || stored == [start.as_str()] {
            continue;
        }
        out.insert(start.clone(), stored.into_iter().map(NamedNode::new_unchecked).collect());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::Triple;

    fn n(s: &str) -> NamedNode {
        NamedNode::new_unchecked(format!("http://e/{s}"))
    }

    fn t(s: &str, p: &str, o: &str) -> Triple {
        Triple::new(n(s), NamedNode::new_unchecked(p), n(o))
    }

    fn index(triples: &[Triple]) -> Index {
        Index::build(triples.iter().map(Triple::as_ref), &["http://e/base/"])
    }

    fn names(found: Targets<'_>) -> Vec<String> {
        match found {
            Targets::Same => vec![],
            Targets::One(one) => vec![one.as_str().replace("http://e/", "")],
            Targets::Many(many) => many.iter().map(|m| m.as_str().replace("http://e/", "")).collect(),
        }
    }

    #[test]
    fn a_shared_name_with_one_bridge_is_a_swap_and_with_none_is_left_alone() {
        let i = index(&[t("pan/title", EQUIVALENT_PROPERTY, "base/title"), t("base/abstract", SUB_PROPERTY, "base/top")]);
        assert_eq!(names(i.property(n("base/title").as_ref())), ["pan/title"]);
        // The stored name already asks for itself.
        assert_eq!(i.property(n("pan/title").as_ref()), Targets::Same);
        // No stored name bridges to it: asked as written, and matches nothing.
        assert_eq!(i.property(n("base/abstract").as_ref()), Targets::Same);
        assert_eq!(i.property(n("unheard/of").as_ref()), Targets::Same);
    }

    #[test]
    fn equivalents_reach_each_other_through_the_shared_name() {
        let i = index(&[
            t("pan/title", EQUIVALENT_PROPERTY, "base/title"),
            t("lex/title", EQUIVALENT_PROPERTY, "base/title"),
            t("lex/headline", SUB_PROPERTY, "lex/title"),
        ]);
        assert_eq!(names(i.property(n("base/title").as_ref())), ["lex/headline", "lex/title", "pan/title"]);
        assert_eq!(names(i.property(n("pan/title").as_ref())), ["lex/headline", "lex/title", "pan/title"]);
        // Downward only: the sub-property does not ask for its parent.
        assert_eq!(i.property(n("lex/headline").as_ref()), Targets::Same);
    }

    #[test]
    fn a_class_asks_for_its_subclasses_at_every_depth() {
        let i = index(&[
            t("pan/Media", SUB_CLASS, "base/Thing"),
            t("pan/Image", SUB_CLASS, "pan/Media"),
            t("pan/Video", SUB_CLASS, "pan/Media"),
            t("pan/Commit", SUB_CLASS, "other/Root"),
        ]);
        assert_eq!(names(i.class(n("base/Thing").as_ref())), ["pan/Image", "pan/Media", "pan/Video"]);
        assert_eq!(names(i.class(n("pan/Media").as_ref())), ["pan/Image", "pan/Media", "pan/Video"]);
        assert_eq!(i.class(n("pan/Image").as_ref()), Targets::Same);
        // Properties and classes are separate: a class edge is no property edge.
        assert_eq!(i.property(n("pan/Media").as_ref()), Targets::Same);
    }

    #[test]
    fn rdf_type_is_never_swapped_and_a_loop_ends() {
        let rdf_type = NamedNode::new_unchecked(RDF_TYPE);
        let i = Index::build(
            [
                Triple::new(n("a"), NamedNode::new_unchecked(SUB_PROPERTY), rdf_type.clone()).as_ref(),
                t("x", SUB_CLASS, "y").as_ref(),
                t("y", SUB_CLASS, "x").as_ref(),
            ],
            &[],
        );
        assert_eq!(i.property(rdf_type.as_ref()), Targets::Same);
        assert_eq!(names(i.class(n("x").as_ref())), ["x", "y"]);
    }
}
