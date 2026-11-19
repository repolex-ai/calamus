//! The short addresses of the Subtexture stack, and the table of store names
//! they are read against. This is the one part of calamus that belongs to a
//! particular stack. A host with no such addresses does not call it.
//!
//! A thing's full address is
//!
//!   https://repolex.ai/{squad}/{user}/{namespace}/{Class}/{id}
//!
//! and a query or a document may write it short (goodlux, 2026-11-18):
//!
//!   <{namespace}/{Class}/{id}>                 in the store being asked
//!   <{user}/{namespace}/{Class}/{id}>          in another store, same squad
//!   <{squad}/{user}/{namespace}/{Class}/{id}>  in a store of another squad
//!
//! The first form is ordinary relative resolution: the host parses the query
//! with the base of the store BEING ASKED (`Names::base`), never the
//! caller's. The other two are not: a standard parser reads
//! `<s3lk13/copia/Moment/x>` as a path inside the store being asked. So
//! after parsing, `Names::apply` finds every address that landed under the
//! base with a store's or a squad's name as its first part, and moves it to
//! where it was meant.
//!
//! Which form an address is cannot be told by counting its parts, because a
//! file's address (`git-lex/File/Soul/Note/x.md`) has as many as it has
//! folders. It is told by the FIRST part: a namespace the store has
//! installed means this store, a store's name means that store, a squad's
//! name means that squad. That only works if no name is two of those at
//! once, which `Names::check` holds the host to.
//!
//! The table is handed in by the host, in memory. calamus reads no file and
//! asks no service: git-lex fills it from the machine's registry, a Pan
//! running alone hands in a table of one, and Syrinx fills it from what each
//! daemon reports. Who may claim a name, and the key that proves it, is the
//! registry's business and is checked where the registry is, not here.

use crate::walk::{self, Visitor};
use crate::Result;
use oxrdf::NamedNode;
use spargebra::term::TermPattern;
use spargebra::Query;

/// Whether a store is one soul's own, or one set aside for sharing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Personal,
    Shared,
}

/// One store the host knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    pub squad: String,
    pub name: String,
    /// The full id: the 40 characters of the repo's first commit. Never part
    /// of an address; here so a host can go from a name to the id its
    /// daemons and tokens use.
    pub id: String,
    pub kind: Kind,
}

/// What the host knows about names, for one store being asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    /// Where every address starts: `https://repolex.ai/`.
    pub root: String,
    /// The squad and name of the store being asked.
    pub squad: String,
    pub user: String,
    /// The namespaces installed in the store being asked (`soul`, `copia`,
    /// `git-lex`, ...). An address starting with one of these is in this store.
    pub namespaces: Vec<String>,
    /// Every store the host knows, this one included.
    pub stores: Vec<Store>,
}

/// A store's or a squad's name: one word of lowercase letters and digits,
/// with at least one letter from g to z, so that it can never be read as the
/// start of a hexadecimal id.
pub fn is_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) && s.bytes().any(|b| (b'g'..=b'z').contains(&b))
}

/// A document's id: lowercase letters and digits in groups joined by single
/// dashes. No slash, no dot, no capital.
pub fn is_document_id(s: &str) -> bool {
    !s.is_empty() && s.split('-').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

/// Names that can never be a store's or a squad's.
pub const RESERVED: &[&str] = &["ontology"];

impl Names {
    /// The base to parse with: `https://repolex.ai/{squad}/{user}/`.
    pub fn base(&self) -> String {
        format!("{}{}/{}/", self.root, self.squad, self.user)
    }

    /// The store with this name in the squad of the store being asked.
    pub fn store(&self, name: &str) -> Option<&Store> {
        self.stores.iter().find(|s| s.squad == self.squad && s.name == name)
    }

    /// Hold the table to the rules that make the first part of an address
    /// mean one thing. A host calls this when it builds the table and
    /// refuses to serve if it fails.
    pub fn check(&self) -> Result<()> {
        if !self.root.ends_with('/') {
            return Err(format!("the root address '{}' must end with a slash", self.root).into());
        }
        if self.store(&self.user).is_none() {
            return Err(format!("the store being asked, '{}/{}', is not in the table of stores", self.squad, self.user).into());
        }
        for (i, s) in self.stores.iter().enumerate() {
            for (what, word) in [("store", &s.name), ("squad", &s.squad)] {
                if !is_name(word) {
                    return Err(format!("'{word}' cannot be a {what}'s name: a name is one word of lowercase letters and digits with at least one letter from g to z").into());
                }
                if RESERVED.contains(&word.as_str()) {
                    return Err(format!("'{word}' cannot be a {what}'s name: the word is reserved").into());
                }
                if self.namespaces.iter().any(|n| n == word) {
                    return Err(format!("'{word}' is a {what}'s name and also a namespace installed in this store, so an address starting with it could mean either").into());
                }
            }
            if self.stores.iter().any(|o| o.squad == s.name) {
                return Err(format!("'{}' is a store's name and also a squad's name, so an address starting with it could mean either", s.name).into());
            }
            if self.stores[..i].iter().any(|o| o.squad == s.squad && o.name == s.name) {
                return Err(format!("the store '{}/{}' is in the table twice", s.squad, s.name).into());
            }
        }
        Ok(())
    }

    /// Where an address that was parsed against the base was meant to point,
    /// if that is somewhere else. `None` means it is right as it stands.
    pub fn resolve(&self, address: &str) -> Result<Option<String>> {
        let base = self.base();
        let Some(rest) = address.strip_prefix(base.as_str()) else { return Ok(None) };
        let mut parts = rest.splitn(3, '/');
        let (Some(first), Some(second)) = (parts.next(), parts.next()) else { return Ok(None) };
        if self.namespaces.iter().any(|n| n == first) {
            return Ok(None);
        }
        if self.store(first).is_some() {
            return Ok(Some(format!("{}{}/{rest}", self.root, self.squad)));
        }
        if self.stores.iter().any(|s| s.squad == first) {
            if self.stores.iter().any(|s| s.squad == first && s.name == second) {
                return Ok(Some(format!("{}{rest}", self.root)));
            }
            return Err(format!("The address <{rest}> names the squad '{first}', which has no store named '{second}'").into());
        }
        Ok(None)
    }

    /// Move every short address in the query that names another store.
    /// Returns how many were moved. A SERVICE block is left alone: it is
    /// read against the base of the store it is sent to, by that store.
    pub fn apply(&self, query: &mut Query) -> Result<usize> {
        let mut mover = Mover { names: self, moved: 0, failed: None };
        match query {
            Query::Select { pattern, .. } | Query::Describe { pattern, .. } | Query::Ask { pattern, .. } => walk::pattern(pattern, &mut mover)?,
            Query::Construct { template, pattern, .. } => {
                for triple in template.iter_mut() {
                    walk::triple(triple, &mut mover);
                }
                walk::pattern(pattern, &mut mover)?;
            }
        }
        match mover.failed {
            Some(e) => Err(e),
            None => Ok(mover.moved),
        }
    }
}

struct Mover<'a> {
    names: &'a Names,
    moved: usize,
    failed: Option<crate::Error>,
}

impl Mover<'_> {
    fn node(&mut self, node: &mut NamedNode) {
        match self.names.resolve(node.as_str()) {
            Ok(None) => {}
            Ok(Some(meant)) => {
                *node = NamedNode::new_unchecked(meant);
                self.moved += 1;
            }
            Err(e) => self.failed = Some(e),
        }
    }
}

impl Visitor for Mover<'_> {
    fn term(&mut self, term: &mut TermPattern) {
        if let TermPattern::NamedNode(node) = term {
            self.node(node);
        }
    }

    fn value(&mut self, node: &mut NamedNode) {
        self.node(node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spargebra::SparqlParser;

    fn store(squad: &str, name: &str, kind: Kind) -> Store {
        Store { squad: squad.into(), name: name.into(), id: "b5f080a1bd3c1c143c7397f8835157363e990d7b".into(), kind }
    }

    fn names() -> Names {
        Names {
            root: "https://repolex.ai/".into(),
            squad: "7r1pl3f0rc3".into(),
            user: "m4xx".into(),
            namespaces: vec!["soul".into(), "copia".into(), "git-lex".into()],
            stores: vec![
                store("7r1pl3f0rc3", "m4xx", Kind::Personal),
                store("7r1pl3f0rc3", "s3lk13", Kind::Personal),
                store("7r1pl3f0rc3", "commons", Kind::Shared),
                store("demolishous", "tr1pl3x", Kind::Personal),
            ],
        }
    }

    fn applied(sparql: &str) -> String {
        let n = names();
        n.check().unwrap();
        let mut query = SparqlParser::new().with_base_iri(n.base()).unwrap().parse_query(sparql).unwrap();
        n.apply(&mut query).unwrap();
        query.to_string()
    }

    #[test]
    fn the_first_part_of_an_address_says_which_store() {
        let n = names();
        let at = |short: &str| n.resolve(&format!("{}{short}", n.base())).unwrap();
        // This store: a namespace it has installed. Right as it stands.
        assert_eq!(at("soul/Note/day-8"), None);
        // A file's address has many parts and is still this store's.
        assert_eq!(at("git-lex/File/Soul/Query/recent.md"), None);
        // Another store in the same squad, and this store by its own name.
        assert_eq!(at("s3lk13/copia/Moment/hcqgb789pbtv").as_deref(), Some("https://repolex.ai/7r1pl3f0rc3/s3lk13/copia/Moment/hcqgb789pbtv"));
        assert_eq!(at("commons/pan/Image/x").as_deref(), Some("https://repolex.ai/7r1pl3f0rc3/commons/pan/Image/x"));
        assert_eq!(at("m4xx/soul/Note/day-8").as_deref(), Some("https://repolex.ai/7r1pl3f0rc3/m4xx/soul/Note/day-8"));
        // A store in another squad.
        assert_eq!(at("demolishous/tr1pl3x/soul/Note/a").as_deref(), Some("https://repolex.ai/demolishous/tr1pl3x/soul/Note/a"));
        // A squad that is known, with a store that is not: refused by name.
        assert!(n.resolve(&format!("{}demolishous/nobody/soul/Note/a", n.base())).unwrap_err().0.contains("no store named 'nobody'"));
        // A first part nobody has registered is left where it is.
        assert_eq!(at("pan/Image/x"), None);
        // An address that is not under the base at all is not ours to move.
        assert_eq!(n.resolve("https://repolex.ai/ontology/soul/Note").unwrap(), None);
        assert_eq!(n.resolve("http://example.org/s3lk13/x").unwrap(), None);
    }

    #[test]
    fn short_addresses_are_moved_everywhere_a_value_can_stand() {
        let moved = applied(
            "SELECT ?p WHERE { <soul/Note/a> ?p <s3lk13/copia/Moment/b> . ?r ?q <<( <commons/pan/Image/c> ?p ?o )>> \
             FILTER(?o != <demolishous/tr1pl3x/soul/Note/d>) VALUES ?o { <s3lk13/soul/Note/e> } }",
        );
        for full in [
            "<https://repolex.ai/7r1pl3f0rc3/m4xx/soul/Note/a>",
            "<https://repolex.ai/7r1pl3f0rc3/s3lk13/copia/Moment/b>",
            "<https://repolex.ai/7r1pl3f0rc3/commons/pan/Image/c>",
            "<https://repolex.ai/demolishous/tr1pl3x/soul/Note/d>",
            "<https://repolex.ai/7r1pl3f0rc3/s3lk13/soul/Note/e>",
        ] {
            assert!(moved.contains(full), "{full} not in {moved}");
        }
    }

    #[test]
    fn a_service_block_is_left_for_the_store_it_is_sent_to() {
        let moved = applied("SELECT ?p WHERE { SERVICE <https://repolex.ai/ontology/pan/> { <s3lk13/copia/Moment/b> ?p ?o } }");
        assert!(moved.contains("<https://repolex.ai/7r1pl3f0rc3/m4xx/s3lk13/copia/Moment/b>"), "{moved}");
    }

    #[test]
    fn a_name_is_held_to_one_meaning() {
        assert!(is_name("m4xx") && is_name("7r1pl3f0rc3") && is_name("s3lk13"));
        // All hexadecimal, a dash, a capital, a dot: none is a name.
        for bad in ["4rx".replace('r', "a").replace('x', "b").as_str(), "abc123", "tr1p-l3x", "W4R3Z", "tr1p.l3x", ""] {
            assert!(!is_name(bad), "{bad}");
        }
        assert!(is_document_id("day-8") && is_document_id("2026-11-19-a-note"));
        for bad in ["Day-8", "a--b", "-a", "a/b", "a.md", ""] {
            assert!(!is_document_id(bad), "{bad}");
        }
        let broken = |change: fn(&mut Names)| {
            let mut n = names();
            change(&mut n);
            n.check().unwrap_err().0
        };
        assert!(broken(|n| n.namespaces.push("commons".into())).contains("also a namespace"));
        assert!(broken(|n| n.stores.push(store("s3lk13", "other", Kind::Shared))).contains("also a squad's name"));
        assert!(broken(|n| n.stores.push(store("7r1pl3f0rc3", "ontology", Kind::Shared))).contains("reserved"));
        assert!(broken(|n| n.stores.push(store("7r1pl3f0rc3", "abc123", Kind::Shared))).contains("g to z"));
        assert!(broken(|n| n.stores.push(store("7r1pl3f0rc3", "s3lk13", Kind::Shared))).contains("twice"));
        assert!(broken(|n| n.user = "nobody".into()).contains("not in the table"));
    }
}
