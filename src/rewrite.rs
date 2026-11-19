//! Change a query so it asks for every spelling the ontology says is the
//! same thing. The store is not touched and nothing is stored twice.
//!
//! WHAT IS CHANGED, inside the query's pattern:
//!
//!   - `?s <P> ?o` where `<P>` is a property the index knows;
//!   - `?s rdf:type <C>` where `<C>` is a class the index knows;
//!   - a property inside a property path, and inside a negated set `!(...)`;
//!   - all of the above inside a quoted statement `<<( ?s <P> ?o )>>`, at any
//!     depth, and inside `FILTER EXISTS { ... }`.
//!
//! One name for one name is a swap in place: no extra join, no extra cost.
//!
//! One name for several ("a fan-out") is asked as
//! `{ SELECT DISTINCT <the pattern's own variables> WHERE { ... VALUES ... } }`.
//! The DISTINCT is the point: a thing that carries two spellings of one
//! property, or two kinds under one class, is one answer, as it would be if
//! the store reasoned. It covers that one pattern's variables only, so rows
//! the caller's query has twice for its own reasons stay twice. The variable
//! invented for the VALUES stays inside that sub-select and is never part of
//! an answer. In a property path a fan-out is written as an alternative
//! (`p1|p2`) and no variable is invented at all.
//!
//! WHAT IS NOT CHANGED:
//!
//!   - a predicate or a class that is a variable (`?s ?p ?o`, `?s a ?c`):
//!     it returns what is stored, with nothing added;
//!   - expressions: `FILTER(?p = <P>)` compares against stored spellings.
//!     A name found there that the index has other spellings for is listed
//!     in `Report::left_as_written`, so the host can say so;
//!   - anything inside `SERVICE { }`: the store it is sent to rewrites it
//!     against its own ontology;
//!   - anything inside `GRAPH <g> { }` for a graph the host says holds an
//!     ontology, so a question about the ontology sees it as written;
//!   - a CONSTRUCT template, which is what a query writes out;
//!   - updates. calamus does not take them.
//!
//! An engine's own working queries should not come through here. They use
//! stored spellings and must not change when an ontology gains a name.

use crate::ontology::{Index, Targets, RDF_TYPE};
use crate::walk::{self, Visitor};
use crate::Result;
use oxrdf::{BlankNode, NamedNode, Variable};
use spargebra::algebra::{AggregateExpression, Expression, GraphPattern, OrderExpression, PropertyPathExpression};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};
use spargebra::Query;
use std::collections::HashMap;

/// Which named graphs hold an ontology and are read as written.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// A graph whose name starts with one of these is not rewritten.
    pub ontology_graph_prefixes: Vec<String>,
    /// A graph with exactly one of these names is not rewritten.
    pub ontology_graphs: Vec<String>,
}

/// What a rewrite did, for a host that tells its caller what was run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Names replaced by one other name.
    pub swapped: usize,
    /// Names replaced by two or more.
    pub fanned_out: usize,
    /// Names the index has other spellings for, found where nothing is
    /// rewritten: as a value in an expression, such as
    /// `FILTER(?p = <P>)` or `FILTER(?q = <<( ?s <P> ?o )>>)`. There they are
    /// compared with what is stored, as written, and will most likely match
    /// nothing. Each name once, in address order. A host should tell its
    /// caller, because the answer itself will not.
    pub left_as_written: Vec<NamedNode>,
}

impl Report {
    pub fn changed(&self) -> bool {
        self.swapped + self.fanned_out > 0
    }
}

/// Rewrite `query` in place. On an error the query may be half-changed and
/// must not be run.
pub fn rewrite(query: &mut Query, index: &Index, options: &Options) -> Result<Report> {
    if index.is_empty() {
        return Ok(Report::default());
    }
    // A prefix no name in the query contains, so an invented variable can
    // never meet one the caller wrote.
    let text = query.to_string();
    let mut prefix = String::from("_ql_");
    while text.contains(&prefix) {
        prefix.push('_');
    }
    let mut r = Rewriter { index, options, prefix, minted: 0, blank: HashMap::new(), graphs: Vec::new(), report: Report::default() };
    let pattern = match query {
        Query::Select { pattern, .. } | Query::Describe { pattern, .. } | Query::Ask { pattern, .. } | Query::Construct { pattern, .. } => pattern,
    };
    r.pattern(pattern)?;
    if !r.blank.is_empty() {
        walk::pattern(pattern, &mut Named { blank: &r.blank })?;
    }
    r.report.left_as_written.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    r.report.left_as_written.dedup();
    Ok(r.report)
}

/// A blank node in a pattern is an unnamed variable. One that sits in a
/// fanned-out pattern has to be listed by that pattern's sub-select to keep
/// joining with the rest, and only a variable can be listed, so it is given
/// a name everywhere it appears.
struct Named<'a> {
    blank: &'a HashMap<BlankNode, Variable>,
}

impl Visitor for Named<'_> {
    fn term(&mut self, term: &mut TermPattern) {
        if let TermPattern::BlankNode(b) = term {
            if let Some(v) = self.blank.get(b) {
                *term = TermPattern::Variable(v.clone());
            }
        }
    }
}

struct Rewriter<'a> {
    index: &'a Index,
    options: &'a Options,
    prefix: String,
    minted: usize,
    blank: HashMap<BlankNode, Variable>,
    /// The variables of the GRAPH blocks the walk is inside, outermost first.
    graphs: Vec<Variable>,
    report: Report,
}

/// One name that became several: the variable standing in for it, and what
/// it may be.
type FanOut = (Variable, Vec<NamedNode>);

fn empty() -> GraphPattern {
    GraphPattern::Bgp { patterns: Vec::new() }
}

fn join(left: GraphPattern, right: GraphPattern) -> GraphPattern {
    GraphPattern::Join { left: Box::new(left), right: Box::new(right) }
}

impl Rewriter<'_> {
    fn mint(&mut self) -> Variable {
        self.minted += 1;
        Variable::new_unchecked(format!("{}{}", self.prefix, self.minted - 1))
    }

    fn is_ontology_graph(&self, name: &str) -> bool {
        self.options.ontology_graphs.iter().any(|g| g == name) || self.options.ontology_graph_prefixes.iter().any(|p| name.starts_with(p.as_str()))
    }

    /// The variables a pattern's sub-select has to list: every variable in
    /// these terms, and every blank node, named.
    fn own(&mut self, term: &mut TermPattern, out: &mut Vec<Variable>) {
        match term {
            TermPattern::Variable(v) => out.push(v.clone()),
            TermPattern::BlankNode(b) => {
                let v = match self.blank.get(b) {
                    Some(v) => v.clone(),
                    None => {
                        let v = self.mint();
                        self.blank.insert(b.clone(), v.clone());
                        v
                    }
                };
                *term = TermPattern::Variable(v.clone());
                out.push(v);
            }
            TermPattern::Triple(inner) => {
                self.own(&mut inner.subject, out);
                if let NamedNodePattern::Variable(v) = &inner.predicate {
                    out.push(v.clone());
                }
                self.own(&mut inner.object, out);
            }
            TermPattern::NamedNode(_) | TermPattern::Literal(_) => {}
        }
    }

    /// Wrap one fanned-out pattern so that it gives each answer once and
    /// nothing invented for it is seen outside.
    fn wrap(&mut self, inner: GraphPattern, mut listed: Vec<Variable>, fans: Vec<FanOut>) -> GraphPattern {
        let mut invented = Vec::new();
        let mut inner = inner;
        for (variable, names) in fans {
            let values = GraphPattern::Values { variables: vec![variable.clone()], bindings: names.into_iter().map(|n| vec![Some(GroundTerm::NamedNode(n))]).collect() };
            inner = join(values, inner);
            invented.push(variable);
        }
        // Inside GRAPH ?g, list ?g too. oxigraph binds a GRAPH variable only
        // through the patterns it can see, and it cannot see into a
        // subquery that does not list the variable: without this, a block
        // whose every pattern fanned out came back with ?g unbound (found
        // in Syrinx, 2026-11-19; tested against oxigraph 0.5.9).
        listed.extend(self.graphs.iter().cloned());
        listed.retain(|v| !invented.contains(v));
        listed.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        listed.dedup();
        if listed.is_empty() {
            // Nothing to list: the pattern is a yes or no. One row says yes.
            // (A select that lists nothing cannot be written as SPARQL text.)
            return GraphPattern::Slice { inner: Box::new(GraphPattern::Project { inner: Box::new(inner), variables: invented }), start: 0, length: Some(1) };
        }
        GraphPattern::Distinct { inner: Box::new(GraphPattern::Project { inner: Box::new(inner), variables: listed }) }
    }

    fn term(&mut self, term: &mut TermPattern, fans: &mut Vec<FanOut>) {
        if let TermPattern::Triple(inner) = term {
            self.triple(inner, fans);
        }
    }

    fn triple(&mut self, t: &mut TriplePattern, fans: &mut Vec<FanOut>) {
        self.term(&mut t.subject, fans);
        self.term(&mut t.object, fans);
        let NamedNodePattern::NamedNode(p) = &t.predicate else { return };
        if p.as_str() == RDF_TYPE {
            let TermPattern::NamedNode(class) = &t.object else { return };
            match self.index.class(class.as_ref()) {
                Targets::Same => {}
                Targets::One(one) => {
                    t.object = TermPattern::NamedNode(one.clone());
                    self.report.swapped += 1;
                }
                Targets::Many(many) => {
                    let v = self.mint();
                    fans.push((v.clone(), many.to_vec()));
                    t.object = TermPattern::Variable(v);
                    self.report.fanned_out += 1;
                }
            }
            return;
        }
        match self.index.property(p.as_ref()) {
            Targets::Same => {}
            Targets::One(one) => {
                t.predicate = NamedNodePattern::NamedNode(one.clone());
                self.report.swapped += 1;
            }
            Targets::Many(many) => {
                let v = self.mint();
                fans.push((v.clone(), many.to_vec()));
                t.predicate = NamedNodePattern::Variable(v);
                self.report.fanned_out += 1;
            }
        }
    }

    /// Returns whether a name in the path became an alternative of several.
    fn path(&mut self, path: &mut PropertyPathExpression) -> bool {
        match path {
            PropertyPathExpression::NamedNode(p) => match self.index.property(p.as_ref()) {
                Targets::Same => false,
                Targets::One(one) => {
                    *p = one.clone();
                    self.report.swapped += 1;
                    false
                }
                Targets::Many(many) => {
                    let mut names = many.iter().cloned().map(PropertyPathExpression::NamedNode);
                    let first = names.next().expect("Many is two or more");
                    *path = names.fold(first, |so_far, next| PropertyPathExpression::Alternative(Box::new(so_far), Box::new(next)));
                    self.report.fanned_out += 1;
                    true
                }
            },
            PropertyPathExpression::Reverse(a) | PropertyPathExpression::ZeroOrMore(a) | PropertyPathExpression::OneOrMore(a) | PropertyPathExpression::ZeroOrOne(a) => self.path(a),
            PropertyPathExpression::Sequence(a, b) | PropertyPathExpression::Alternative(a, b) => {
                let (a, b) = (self.path(a), self.path(b));
                a || b
            }
            // "Any property but these" has to leave out every spelling of
            // each. A match is one statement, so nothing comes back twice.
            PropertyPathExpression::NegatedPropertySet(set) => {
                let mut out: Vec<NamedNode> = Vec::new();
                for p in set.iter() {
                    match self.index.property(p.as_ref()) {
                        Targets::Same => out.push(p.clone()),
                        Targets::One(one) => {
                            out.push(one.clone());
                            self.report.swapped += 1;
                        }
                        Targets::Many(many) => {
                            out.extend(many.iter().cloned());
                            self.report.fanned_out += 1;
                        }
                    }
                }
                out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                out.dedup();
                *set = out;
                false
            }
        }
    }

    fn expression(&mut self, e: &mut Expression) -> Result<()> {
        match e {
            Expression::NamedNode(n) => {
                if self.index.property(n.as_ref()) != Targets::Same || self.index.class(n.as_ref()) != Targets::Same {
                    self.report.left_as_written.push(n.clone());
                }
            }
            Expression::Literal(_) | Expression::Variable(_) | Expression::Bound(_) => {}
            Expression::Or(a, b)
            | Expression::And(a, b)
            | Expression::Equal(a, b)
            | Expression::SameTerm(a, b)
            | Expression::Greater(a, b)
            | Expression::GreaterOrEqual(a, b)
            | Expression::Less(a, b)
            | Expression::LessOrEqual(a, b)
            | Expression::Add(a, b)
            | Expression::Subtract(a, b)
            | Expression::Multiply(a, b)
            | Expression::Divide(a, b) => {
                self.expression(a)?;
                self.expression(b)?;
            }
            Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => self.expression(a)?,
            Expression::In(a, list) => {
                self.expression(a)?;
                for item in list {
                    self.expression(item)?;
                }
            }
            Expression::Exists(p) => self.pattern(p)?,
            Expression::If(a, b, c) => {
                self.expression(a)?;
                self.expression(b)?;
                self.expression(c)?;
            }
            Expression::Coalesce(list) | Expression::FunctionCall(_, list) => {
                for item in list {
                    self.expression(item)?;
                }
            }
        }
        Ok(())
    }

    fn pattern(&mut self, p: &mut GraphPattern) -> Result<()> {
        match p {
            GraphPattern::Bgp { patterns } => {
                let mut kept = Vec::new();
                let mut wrapped = Vec::new();
                for mut t in std::mem::take(patterns) {
                    let mut fans = Vec::new();
                    self.triple(&mut t, &mut fans);
                    if fans.is_empty() {
                        kept.push(t);
                        continue;
                    }
                    let mut listed = Vec::new();
                    self.own(&mut t.subject, &mut listed);
                    if let NamedNodePattern::Variable(v) = &t.predicate {
                        listed.push(v.clone());
                    }
                    self.own(&mut t.object, &mut listed);
                    wrapped.push(self.wrap(GraphPattern::Bgp { patterns: vec![t] }, listed, fans));
                }
                let mut all = wrapped.into_iter();
                *p = match (kept.is_empty(), all.next()) {
                    (_, None) => GraphPattern::Bgp { patterns: kept },
                    (true, Some(first)) => all.fold(first, join),
                    (false, Some(first)) => all.fold(join(GraphPattern::Bgp { patterns: kept }, first), join),
                };
            }
            GraphPattern::Path { subject, path, object } => {
                let mut fans = Vec::new();
                self.term(subject, &mut fans);
                self.term(object, &mut fans);
                if self.path(path) || !fans.is_empty() {
                    let mut listed = Vec::new();
                    self.own(subject, &mut listed);
                    self.own(object, &mut listed);
                    let taken = std::mem::replace(p, empty());
                    *p = self.wrap(taken, listed, fans);
                }
            }
            GraphPattern::Join { left, right } | GraphPattern::Union { left, right } | GraphPattern::Minus { left, right } => {
                self.pattern(left)?;
                self.pattern(right)?;
            }
            #[cfg(feature = "sep-0006")]
            GraphPattern::Lateral { left, right } => {
                self.pattern(left)?;
                self.pattern(right)?;
            }
            GraphPattern::LeftJoin { left, right, expression } => {
                self.pattern(left)?;
                self.pattern(right)?;
                if let Some(e) = expression {
                    self.expression(e)?;
                }
            }
            GraphPattern::Filter { expr, inner } => {
                self.expression(expr)?;
                self.pattern(inner)?;
            }
            GraphPattern::Graph { name, inner } => {
                let ontology = matches!(name, NamedNodePattern::NamedNode(g) if self.is_ontology_graph(g.as_str()));
                if !ontology {
                    let named = matches!(name, NamedNodePattern::Variable(_));
                    if let NamedNodePattern::Variable(v) = name {
                        self.graphs.push(v.clone());
                    }
                    let walked = self.pattern(inner);
                    if named {
                        self.graphs.pop();
                    }
                    walked?;
                }
            }
            GraphPattern::Extend { inner, expression, .. } => {
                self.expression(expression)?;
                self.pattern(inner)?;
            }
            GraphPattern::Values { .. } | GraphPattern::Service { .. } => {}
            GraphPattern::OrderBy { inner, expression } => {
                for order in expression {
                    match order {
                        OrderExpression::Asc(e) | OrderExpression::Desc(e) => self.expression(e)?,
                    }
                }
                self.pattern(inner)?;
            }
            GraphPattern::Group { inner, aggregates, .. } => {
                for (_, aggregate) in aggregates.iter_mut() {
                    if let AggregateExpression::FunctionCall { expr, .. } = aggregate {
                        self.expression(expr)?;
                    }
                }
                self.pattern(inner)?;
            }
            GraphPattern::Project { inner, .. } | GraphPattern::Distinct { inner } | GraphPattern::Reduced { inner } | GraphPattern::Slice { inner, .. } => self.pattern(inner)?,
            // A form this code does not know: refuse. Passing it on with a
            // shared name still inside would be an empty answer with no error.
            #[allow(unreachable_patterns)]
            _ => return Err(walk::unknown_form()),
        }
        Ok(())
    }
}
