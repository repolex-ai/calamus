//! calamus prepares a parsed SPARQL query for an oxigraph store.
//!
//! It does three jobs, each on a `spargebra::Query`, the type oxigraph's own
//! parser produces and its evaluator accepts:
//!
//!   - [`rewrite`]: change a query so it asks for every spelling the ontology
//!     says is the same thing (sub-properties, subclasses and equivalents).
//!   - [`bind`]: put a caller's values into a saved query's parameters, as
//!     values in the parsed query and never as text.
//!   - [`names`]: resolve the short addresses that name another store.
//!
//! The order is: parse, bind, names, rewrite, evaluate. [`rewrite`] and
//! [`bind`] know nothing of any one stack. [`names`] is the addressing of the
//! Subtexture stack and is the only part that is.
//!
//! Nothing here reads a file, opens a socket or keeps state between calls.
//! The host hands in what it holds: the ontology as triples, the table of
//! store names, the caller's values.

pub mod bind;
pub mod names;
pub mod ontology;
pub mod rewrite;
mod walk;

pub use ontology::{Index, Targets};
pub use rewrite::{rewrite, Options, Report};

/// Why a query was refused. The text is written for the person who asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error(s.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
