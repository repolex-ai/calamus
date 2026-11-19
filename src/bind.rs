//! Put a caller's values into a saved query's parameters.
//!
//! A saved query declares its parameters, one text each:
//!
//!   name[:kind][=default]
//!
//! The kinds are string, integer, decimal, boolean, date, dateTime and iri.
//!
//!   - No default means the parameter must be given.
//!   - No kind means text, always. Nothing is guessed from how a value
//!     looks. A stated kind is checked.
//!   - A value is put into the parsed query in place of the variable. It is
//!     never pasted into the query's text, so nothing a caller sends can
//!     change what the query asks.
//!   - Every refusal names the parameter.
//!
//! Bind before rewriting: a parameter of kind `iri` used as a class or a
//! property becomes a name only once it is bound, and is then rewritten like
//! any other.
//!
//! The rule and its first tests are w4r3z's (git-lex `src/query_params.rs`,
//! 2026-11-17). This is the copy m4xx wrote for Syrinx from those tests,
//! moved here so there is one.

use crate::Result;
use oxrdf::{Literal, NamedNode, Variable};
use spargebra::algebra::{AggregateExpression, Expression, GraphPattern, OrderExpression};
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};
use spargebra::Query;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    String,
    Integer,
    Decimal,
    Boolean,
    Date,
    DateTime,
    Iri,
}

const KINDS: &[(&str, Kind)] = &[
    ("string", Kind::String),
    ("integer", Kind::Integer),
    ("decimal", Kind::Decimal),
    ("boolean", Kind::Boolean),
    ("date", Kind::Date),
    ("dateTime", Kind::DateTime),
    ("iri", Kind::Iri),
];

impl Kind {
    pub fn word(self) -> &'static str {
        KINDS.iter().find(|(_, k)| *k == self).map(|(w, _)| *w).unwrap_or("string")
    }
}

/// One declared parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub kind: Kind,
    pub default: Option<String>,
}

fn valid_param_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Read one `git-lex:parameter` value: name[:kind][=default]. Split on the
/// first "=", then the left side on ":", so a default may hold either.
pub fn declaration(entry: &str, base: &str) -> Result<Param> {
    let (head, default) = match entry.split_once('=') {
        Some((h, d)) => (h.trim(), Some(d.trim().to_string())),
        None => (entry.trim(), None),
    };
    let (name, kind) = match head.split_once(':') {
        Some((n, k)) => {
            let k = k.trim();
            let Some((_, kind)) = KINDS.iter().find(|(word, _)| *word == k) else {
                let words: Vec<&str> = KINDS.iter().map(|(w, _)| *w).collect();
                return Err(format!("the saved query gives the parameter '{}' the kind '{k}', which is not one. The kinds are: {}", n.trim(), words.join(", ")).into());
            };
            (n.trim(), *kind)
        }
        None => (head, Kind::String),
    };
    let name = name.strip_prefix('?').unwrap_or(name).to_string();
    if !valid_param_name(&name) {
        return Err(format!("the saved query declares a parameter named '{name}'. A name is letters, digits and underscores, starting with a letter").into());
    }
    if let Some(d) = &default {
        read_value(&name, kind, d, base).map_err(|e| format!("the saved query's default is wrong. {e}"))?;
    }
    Ok(Param { name, kind, default })
}

/// Every declared parameter, in name order (the graph keeps no order).
pub fn declarations(entries: &[String], base: &str) -> Result<Vec<Param>> {
    let mut out: Vec<Param> = Vec::new();
    for e in entries {
        let p = declaration(e, base)?;
        if out.iter().any(|o| o.name == p.name) {
            return Err(format!("the saved query declares the parameter '{}' twice", p.name).into());
        }
        out.push(p);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    Iri(NamedNode),
    Literal(Literal),
}

fn typed(text: &str, datatype: &str) -> Term {
    Term::Literal(Literal::new_typed_literal(text, NamedNode::new_unchecked(format!("{XSD}{datatype}"))))
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    s.is_ascii()
        && b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && digits(&s[..4])
        && digits(&s[5..7])
        && digits(&s[8..])
        && matches!(s[5..7].parse::<u8>(), Ok(1..=12))
        && matches!(s[8..].parse::<u8>(), Ok(1..=31))
}

/// hh:mm:ss, an optional fraction, an optional Z or +hh:mm / -hh:mm.
fn is_time(s: &str) -> bool {
    let b = s.as_bytes();
    if !s.is_ascii() || b.len() < 8 || b[2] != b':' || b[5] != b':' || !digits(&s[..2]) || !digits(&s[3..5]) || !digits(&s[6..8]) {
        return false;
    }
    let mut rest = &s[8..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let n = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return false;
        }
        rest = &fraction[n..];
    }
    match rest {
        "" | "Z" => true,
        zone => {
            let z = zone.as_bytes();
            z.len() == 6 && (z[0] == b'+' || z[0] == b'-') && z[3] == b':' && digits(&zone[1..3]) && digits(&zone[4..])
        }
    }
}

/// Read a value as the kind its parameter declares. Text is taken exactly as
/// written. Every other kind is checked.
pub fn read_value(name: &str, kind: Kind, raw: &str, base: &str) -> Result<Term> {
    let wrong = |takes: &str| crate::Error(format!("The parameter '{name}' takes {takes}, and was given '{raw}'"));
    let unsigned = raw.strip_prefix('-').unwrap_or(raw);
    match kind {
        Kind::String => Ok(Term::Literal(Literal::new_simple_literal(raw))),
        Kind::Integer if digits(unsigned) => Ok(typed(raw, "integer")),
        Kind::Integer => Err(wrong("a whole number")),
        Kind::Decimal => match unsigned.split_once('.') {
            Some((whole, fraction)) if digits(whole) && digits(fraction) => Ok(typed(raw, "decimal")),
            None if digits(unsigned) => Ok(typed(raw, "decimal")),
            _ => Err(wrong("a number, such as 1.5")),
        },
        Kind::Boolean if raw == "true" || raw == "false" => Ok(typed(raw, "boolean")),
        Kind::Boolean => Err(wrong("true or false")),
        Kind::Date if is_date(raw) => Ok(typed(raw, "date")),
        Kind::Date => Err(wrong("a date, such as 2026-11-17")),
        Kind::DateTime if raw.is_ascii() && raw.len() > 11 && is_date(&raw[..10]) && raw.as_bytes()[10] == b'T' && is_time(&raw[11..]) => Ok(typed(raw, "dateTime")),
        Kind::DateTime => Err(wrong("a date and time, such as 2026-11-17T09:00:00-08:00")),
        Kind::Iri => {
            let inner = raw.strip_prefix('<').and_then(|r| r.strip_suffix('>')).unwrap_or(raw);
            if inner.is_empty() {
                return Err(wrong("a document's address"));
            }
            let full = if inner.contains("://") || inner.starts_with("urn:") { inner.to_string() } else { format!("{base}{inner}") };
            NamedNode::new(&full).map(Term::Iri).map_err(|_| wrong("a document's address"))
        }
    }
}

fn describe(declared: &[Param]) -> String {
    if declared.is_empty() {
        return "It takes none".to_string();
    }
    let list: Vec<String> = declared
        .iter()
        .map(|p| {
            let kind = if p.kind == Kind::String { String::new() } else { format!("{}, ", p.kind.word()) };
            match &p.default {
                Some(d) => format!("{} ({kind}default {d})", p.name),
                None => format!("{} ({kind}must be given)", p.name),
            }
        })
        .collect();
    format!("It takes: {}", list.join(", "))
}

/// Put every declared parameter into `query` as a value. `given` is what
/// the caller sent, as name and text. `base` is the address a short `iri`
/// value is read against: the base of the store being asked, ending in "/".
/// On an error the query may be half-changed and must not be run.
pub fn bind(query: &mut Query, declared: &[Param], given: &[(String, String)], base: &str) -> Result<()> {
    if declared.is_empty() && given.is_empty() {
        return Ok(());
    }
    if let Some((name, _)) = given.iter().find(|(n, _)| !declared.iter().any(|p| &p.name == n)) {
        return Err(format!("This query has no parameter named '{name}'. {}", describe(declared)).into());
    }
    let mut values: Vec<(Variable, Term)> = Vec::new();
    for p in declared {
        let raw = given.iter().find(|(n, _)| n == &p.name).map(|(_, v)| v.as_str()).or(p.default.as_deref());
        let Some(raw) = raw else {
            return Err(format!("The parameter '{}' is missing and has no default. {}", p.name, describe(declared)).into());
        };
        values.push((Variable::new_unchecked(p.name.clone()), read_value(&p.name, p.kind, raw, base)?));
    }
    let mut binder = Binder { values: &values, used: vec![false; values.len()] };
    match query {
        Query::Select { pattern, .. } | Query::Describe { pattern, .. } | Query::Ask { pattern, .. } => binder.pattern(pattern)?,
        Query::Construct { template, pattern, .. } => {
            for triple in template.iter_mut() {
                binder.triple(triple)?;
            }
            binder.pattern(pattern)?;
        }
    }
    if let Some(i) = binder.used.iter().position(|u| !u) {
        return Err(format!("This query declares the parameter '{}' and never uses it", values[i].0.as_str()).into());
    }
    Ok(())
}

struct Binder<'a> {
    values: &'a [(Variable, Term)],
    used: Vec<bool>,
}

impl Binder<'_> {
    fn value(&mut self, v: &Variable) -> Option<Term> {
        let i = self.values.iter().position(|(var, _)| var == v)?;
        self.used[i] = true;
        Some(self.values[i].1.clone())
    }

    fn is_param(&self, v: &Variable) -> bool {
        self.values.iter().any(|(var, _)| var == v)
    }

    fn must_be_address(v: &Variable, place: &str) -> String {
        format!("The query uses ?{0} as {place}, so the parameter '{0}' must be declared as an address: {0}:iri", v.as_str())
    }

    fn sets_it_itself(v: &Variable) -> String {
        format!("The query gives ?{0} a value itself, so '{0}' cannot also be a parameter", v.as_str())
    }

    fn term(&mut self, t: &mut TermPattern) -> Result<()> {
        match t {
            TermPattern::Variable(v) => {
                if let Some(value) = self.value(v) {
                    *t = match value {
                        Term::Iri(n) => TermPattern::NamedNode(n),
                        Term::Literal(l) => TermPattern::Literal(l),
                    };
                }
            }
            TermPattern::Triple(inner) => self.triple(inner)?,
            _ => {}
        }
        Ok(())
    }

    fn named(&mut self, n: &mut NamedNodePattern, place: &str) -> Result<()> {
        if let NamedNodePattern::Variable(v) = n {
            match self.value(v) {
                Some(Term::Iri(node)) => *n = NamedNodePattern::NamedNode(node),
                Some(Term::Literal(_)) => return Err(Self::must_be_address(v, place).into()),
                None => {}
            }
        }
        Ok(())
    }

    fn triple(&mut self, t: &mut TriplePattern) -> Result<()> {
        self.term(&mut t.subject)?;
        self.named(&mut t.predicate, "a property")?;
        self.term(&mut t.object)
    }

    fn as_expression(value: Term) -> Expression {
        match value {
            Term::Iri(n) => Expression::NamedNode(n),
            Term::Literal(l) => Expression::Literal(l),
        }
    }

    fn expression(&mut self, e: &mut Expression) -> Result<()> {
        match e {
            Expression::NamedNode(_) | Expression::Literal(_) => {}
            Expression::Variable(v) => {
                if let Some(value) = self.value(v) {
                    *e = Self::as_expression(value);
                }
            }
            Expression::Bound(v) => {
                if self.value(v).is_some() {
                    *e = Expression::Literal(Literal::new_typed_literal("true", NamedNode::new_unchecked(format!("{XSD}boolean"))));
                }
            }
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
                for t in patterns {
                    self.triple(t)?;
                }
            }
            GraphPattern::Path { subject, object, .. } => {
                self.term(subject)?;
                self.term(object)?;
            }
            GraphPattern::Join { left, right } | GraphPattern::Union { left, right } | GraphPattern::Minus { left, right } => {
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
                self.named(name, "a graph's name")?;
                self.pattern(inner)?;
            }
            GraphPattern::Service { name, inner, .. } => {
                self.named(name, "a service's address")?;
                self.pattern(inner)?;
            }
            GraphPattern::Extend { inner, variable, expression } => {
                if self.is_param(variable) {
                    return Err(Self::sets_it_itself(variable).into());
                }
                self.expression(expression)?;
                self.pattern(inner)?;
            }
            GraphPattern::Values { variables, .. } => {
                if let Some(v) = variables.iter().find(|v| self.is_param(v)) {
                    return Err(Self::sets_it_itself(v).into());
                }
            }
            GraphPattern::OrderBy { inner, expression } => {
                for order in expression {
                    match order {
                        OrderExpression::Asc(e) | OrderExpression::Desc(e) => self.expression(e)?,
                    }
                }
                self.pattern(inner)?;
            }
            GraphPattern::Project { inner, variables } => {
                self.pattern(inner)?;
                // A parameter the query also returns keeps its column: the
                // value is put back under its name after the pattern ran.
                for v in variables.iter() {
                    if let Some(value) = self.value(v) {
                        let taken = std::mem::replace(inner.as_mut(), GraphPattern::Bgp { patterns: Vec::new() });
                        **inner = GraphPattern::Extend { inner: Box::new(taken), variable: v.clone(), expression: Self::as_expression(value) };
                    }
                }
            }
            GraphPattern::Distinct { inner } | GraphPattern::Reduced { inner } | GraphPattern::Slice { inner, .. } => self.pattern(inner)?,
            GraphPattern::Group { inner, variables, aggregates } => {
                for (target, aggregate) in aggregates.iter_mut() {
                    if self.is_param(target) {
                        return Err(Self::sets_it_itself(target).into());
                    }
                    if let AggregateExpression::FunctionCall { expr, .. } = aggregate {
                        self.expression(expr)?;
                    }
                }
                // Grouping by a fixed value groups by nothing.
                variables.retain(|v| !self.values.iter().any(|(var, _)| var == v));
                self.pattern(inner)?;
            }
            // A form this code does not know: refuse, never skip it and
            // leave a parameter unfilled inside.
            #[allow(unreachable_patterns)]
            _ => return Err("The query uses a form of SPARQL that parameters cannot be put into yet".into()),
        }
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use spargebra::SparqlParser;

    const BASE: &str = "https://repolex.ai/7r1pl3f0rc3/m4xx/";

    fn p(name: &str, kind: Kind, default: Option<&str>) -> Param {
        Param { name: name.into(), kind, default: default.map(str::to_string) }
    }

    fn given(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    /// Parse, bind, and print what the query became.
    fn bound(text: &str, declared: &[Param], given: &[(String, String)]) -> std::result::Result<String, String> {
        let mut query = SparqlParser::new().parse_query(text).unwrap();
        bind(&mut query, declared, given, BASE).map_err(|e| e.0)?;
        Ok(query.to_string())
    }

    const Q: &str = "SELECT ?d WHERE { ?b <http://e/name> ?being ; <http://e/desc> ?d }";

    #[test]
    fn a_declaration_is_name_kind_default() {
        let declaration = |entry: &str| declaration(entry, BASE).map_err(|e| e.0);
        assert_eq!(declaration("being=sylkie").unwrap(), p("being", Kind::String, Some("sylkie")));
        assert_eq!(declaration("count:integer=10").unwrap(), p("count", Kind::Integer, Some("10")));
        assert_eq!(declaration("since:date").unwrap(), p("since", Kind::Date, None));
        assert_eq!(declaration("doc:iri=<soul/Query/recent>").unwrap(), p("doc", Kind::Iri, Some("<soul/Query/recent>")));
        // Split on the first "=", then the left side on ":": a default may hold either.
        assert_eq!(declaration("note=a:b=c").unwrap(), p("note", Kind::String, Some("a:b=c")));
        assert!(declaration("n:number").unwrap_err().contains("The kinds are: string, integer"));
        assert!(declaration("n:integer=ten").unwrap_err().contains("default is wrong"));
        assert!(declaration("two words").unwrap_err().contains("letters, digits"));
        assert!(declarations(&["x".into(), "x=1".into()], BASE).unwrap_err().0.contains("twice"));
        let names: Vec<String> = declarations(&["zed".into(), "alpha=1".into()], BASE).unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["alpha", "zed"]);
    }

    #[test]
    fn a_query_with_no_parameters_is_not_touched() {
        let mut query = SparqlParser::new().parse_query("SELECT * WHERE { ?s ?p ?o }").unwrap();
        let before = query.clone();
        bind(&mut query, &[], &[], BASE).unwrap();
        assert_eq!(query, before);
    }

    #[test]
    fn a_value_goes_in_as_a_value_and_never_as_text() {
        let decl = [p("being", Kind::String, None)];
        let text = bound(Q, &decl, &given(&[("being", "sylkie-glitch")])).unwrap();
        assert!(text.contains("\"sylkie-glitch\""), "{text}");
        assert!(!text.contains("?being"));
        // A value written to break out of the query stays one piece of text.
        let attack = "x\" } . ?s ?p ?o . { ?a ?b \"";
        let mut query = SparqlParser::new().parse_query(Q).unwrap();
        bind(&mut query, &decl, &given(&[("being", attack)]), BASE).unwrap();
        fn statements(p: &GraphPattern) -> usize {
            match p {
                GraphPattern::Bgp { patterns } => patterns.len(),
                GraphPattern::Project { inner, .. } => statements(inner),
                _ => 99,
            }
        }
        let Query::Select { pattern, .. } = &query else { panic!() };
        assert_eq!(statements(pattern), 2, "the value added a pattern: {query}");
        // And printed, it reads back as the same query.
        assert_eq!(SparqlParser::new().parse_query(&query.to_string()).unwrap(), query);
    }

    #[test]
    fn nothing_is_guessed_and_a_stated_kind_is_checked() {
        let read_value = |name: &str, kind: Kind, raw: &str| read_value(name, kind, raw, BASE);
        // No kind: the text 42, not a number.
        let text = bound(Q, &[p("being", Kind::String, None)], &given(&[("being", "42")])).unwrap();
        assert!(text.contains("\"42\"") && !text.contains("integer"), "{text}");
        let q = "SELECT ?s WHERE { ?s <http://e/n> ?n . FILTER(?n > ?count) }";
        let text = bound(q, &[p("count", Kind::Integer, Some("10"))], &[]).unwrap();
        assert!(text.contains("10"), "{text}");
        let e = bound(q, &[p("count", Kind::Integer, None)], &given(&[("count", "ten")])).unwrap_err();
        assert!(e.contains("'count'") && e.contains("whole number"), "{e}");
        assert!(read_value("d", Kind::Date, "2026-11-17").is_ok());
        assert!(read_value("d", Kind::Date, "2026-13-01").is_err());
        assert!(read_value("t", Kind::DateTime, "2026-11-17T09:00:00-08:00").is_ok());
        assert!(read_value("t", Kind::DateTime, "2026-11-17T09:00:00.5Z").is_ok());
        assert!(read_value("t", Kind::DateTime, "2026-11-17 09:00").is_err());
        assert!(read_value("b", Kind::Boolean, "yes").is_err());
        assert!(read_value("x", Kind::Decimal, "-1.5").is_ok());
        // A short address is read against the base of the store being asked.
        assert_eq!(read_value("a", Kind::Iri, "soul/Query/recent").unwrap(), Term::Iri(NamedNode::new_unchecked("https://repolex.ai/7r1pl3f0rc3/m4xx/soul/Query/recent")));
        assert_eq!(read_value("a", Kind::Iri, "<http://e/x>").unwrap(), Term::Iri(NamedNode::new_unchecked("http://e/x")));
    }

    #[test]
    fn every_refusal_names_the_parameter() {
        let decl = [p("being", Kind::String, None)];
        let e = bound(Q, &decl, &given(&[("set", "x"), ("being", "y")])).unwrap_err();
        assert!(e.contains("no parameter named 'set'") && e.contains("being (must be given)"), "{e}");
        let e = bound(Q, &decl, &[]).unwrap_err();
        assert!(e.contains("'being' is missing"), "{e}");
        let e = bound(Q, &[p("being", Kind::String, None), p("extra", Kind::String, Some("1"))], &given(&[("being", "y")])).unwrap_err();
        assert!(e.contains("'extra' and never uses it"), "{e}");
        // Used as a property but not declared an address.
        let e = bound("SELECT ?o WHERE { ?s ?prop ?o }", &[p("prop", Kind::String, None)], &given(&[("prop", "x")])).unwrap_err();
        assert!(e.contains("prop:iri"), "{e}");
        assert!(bound("SELECT ?o WHERE { ?s ?prop ?o }", &[p("prop", Kind::Iri, None)], &given(&[("prop", "<http://e/p>")])).unwrap().contains("<http://e/p>"));
        // The query sets the variable itself.
        let e = bound("SELECT ?x WHERE { BIND(1 AS ?x) }", &[p("x", Kind::String, None)], &given(&[("x", "1")])).unwrap_err();
        assert!(e.contains("gives ?x a value itself"), "{e}");
        let e = bound("SELECT ?x WHERE { VALUES ?x { 1 } }", &[p("x", Kind::String, None)], &given(&[("x", "1")])).unwrap_err();
        assert!(e.contains("gives ?x a value itself"), "{e}");
    }

    #[test]
    fn a_parameter_reaches_every_part_of_the_query() {
        let decl = [p("being", Kind::String, None)];
        let g = given(&[("being", "x")]);
        for q in [
            "SELECT ?d WHERE { ?b <http://e/d> ?d FILTER(?d = ?being) }",
            "SELECT ?d WHERE { ?b <http://e/d> ?d FILTER EXISTS { ?b <http://e/n> ?being } }",
            "SELECT ?d WHERE { { SELECT ?d WHERE { ?b <http://e/n> ?being ; <http://e/d> ?d } } }",
            "SELECT ?d WHERE { SERVICE <https://repolex.ai/ontology/pan/> { ?b <http://e/n> ?being ; <http://e/d> ?d } }",
            "SELECT ?d WHERE { ?b <http://e/n>/<http://e/m> ?being ; <http://e/d> ?d }",
            "SELECT ?d WHERE { ?b <http://e/d> ?d FILTER(BOUND(?being)) }",
            "SELECT ?d WHERE { ?r ?p <<( ?b <http://e/n> ?being )>> ; <http://e/d> ?d }",
        ] {
            let text = bound(q, &decl, &g).unwrap();
            assert!(!text.contains("?being"), "left in: {text}");
        }
        // A parameter that is also returned keeps its column.
        let text = bound("SELECT ?being ?d WHERE { ?b <http://e/n> ?being ; <http://e/d> ?d }", &decl, &g).unwrap();
        assert!(text.contains("AS ?being"), "{text}");
        // Grouped by a parameter: grouped by nothing.
        let text = bound("SELECT (COUNT(?b) AS ?n) WHERE { ?b <http://e/n> ?being } GROUP BY ?being", &decl, &g).unwrap();
        assert!(!text.contains("GROUP BY"), "{text}");
    }
}
