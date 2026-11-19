//! One walk over every place in a query's pattern where a subject, an object
//! or a value can stand. It never enters a SERVICE block: what is inside one
//! is another store's to read.

use crate::Result;
use oxrdf::NamedNode;
use spargebra::algebra::{AggregateExpression, Expression, GraphPattern, OrderExpression};
use spargebra::term::{GroundTerm, TermPattern, TriplePattern};

pub(crate) trait Visitor {
    /// A subject or an object, at any depth inside quoted statements.
    fn term(&mut self, _term: &mut TermPattern) {}
    /// An address used as a value: in an expression or in VALUES.
    fn value(&mut self, _node: &mut NamedNode) {}
}

pub(crate) fn unknown_form() -> crate::Error {
    "The query uses a form of SPARQL that calamus does not know, so it is refused and not passed on half-prepared".into()
}

fn term(t: &mut TermPattern, v: &mut impl Visitor) {
    if let TermPattern::Triple(inner) = t {
        triple(inner, v);
    }
    v.term(t);
}

pub(crate) fn triple(t: &mut TriplePattern, v: &mut impl Visitor) {
    term(&mut t.subject, v);
    term(&mut t.object, v);
}

fn ground(g: &mut GroundTerm, v: &mut impl Visitor) {
    match g {
        GroundTerm::NamedNode(n) => v.value(n),
        GroundTerm::Literal(_) => {}
        GroundTerm::Triple(inner) => {
            v.value(&mut inner.subject);
            ground(&mut inner.object, v);
        }
    }
}

pub(crate) fn expression(e: &mut Expression, v: &mut impl Visitor) -> Result<()> {
    match e {
        Expression::NamedNode(n) => v.value(n),
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
            expression(a, v)?;
            expression(b, v)?;
        }
        Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => expression(a, v)?,
        Expression::In(a, list) => {
            expression(a, v)?;
            for item in list {
                expression(item, v)?;
            }
        }
        Expression::Exists(p) => pattern(p, v)?,
        Expression::If(a, b, c) => {
            expression(a, v)?;
            expression(b, v)?;
            expression(c, v)?;
        }
        Expression::Coalesce(list) | Expression::FunctionCall(_, list) => {
            for item in list {
                expression(item, v)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn pattern(p: &mut GraphPattern, v: &mut impl Visitor) -> Result<()> {
    match p {
        GraphPattern::Bgp { patterns } => {
            for t in patterns {
                triple(t, v);
            }
        }
        GraphPattern::Path { subject, object, .. } => {
            term(subject, v);
            term(object, v);
        }
        GraphPattern::Join { left, right } | GraphPattern::Union { left, right } | GraphPattern::Minus { left, right } => {
            pattern(left, v)?;
            pattern(right, v)?;
        }
        #[cfg(feature = "sep-0006")]
        GraphPattern::Lateral { left, right } => {
            pattern(left, v)?;
            pattern(right, v)?;
        }
        GraphPattern::LeftJoin { left, right, expression: e } => {
            pattern(left, v)?;
            pattern(right, v)?;
            if let Some(e) = e {
                expression(e, v)?;
            }
        }
        GraphPattern::Filter { expr, inner } => {
            expression(expr, v)?;
            pattern(inner, v)?;
        }
        GraphPattern::Extend { inner, expression: e, .. } => {
            expression(e, v)?;
            pattern(inner, v)?;
        }
        GraphPattern::Values { bindings, .. } => {
            for g in bindings.iter_mut().flatten().flatten() {
                ground(g, v);
            }
        }
        GraphPattern::OrderBy { inner, expression: orders } => {
            for order in orders {
                match order {
                    OrderExpression::Asc(e) | OrderExpression::Desc(e) => expression(e, v)?,
                }
            }
            pattern(inner, v)?;
        }
        GraphPattern::Group { inner, aggregates, .. } => {
            for (_, aggregate) in aggregates.iter_mut() {
                if let AggregateExpression::FunctionCall { expr, .. } = aggregate {
                    expression(expr, v)?;
                }
            }
            pattern(inner, v)?;
        }
        GraphPattern::Graph { inner, .. }
        | GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. } => pattern(inner, v)?,
        GraphPattern::Service { .. } => {}
        #[allow(unreachable_patterns)]
        _ => return Err(unknown_form()),
    }
    Ok(())
}
