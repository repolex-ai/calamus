# calamus

calamus prepares a parsed SPARQL query for an [oxigraph](https://github.com/oxigraph/oxigraph) store.

A calamus is a reed. In the myth, the nymph Syrinx became a stand of reeds and Pan cut his pipes from them. This library is the material the stores of the Subtexture stack are cut from: Pan, Syrinx, git-lex, ravel and Horae each call it before they run a query.

It does three jobs. Each one takes a `spargebra::Query`, the type oxigraph's own parser produces and its evaluator accepts, so a prepared query is handed straight to oxigraph and is never printed and parsed again.

| Module | Job | Tied to Subtexture? |
|---|---|---|
| `rewrite` | Change a query so it asks for every spelling the ontology says is the same thing | No |
| `bind` | Put a caller's values into a saved query's parameters, safely | No |
| `names` | Resolve the short addresses that name another store | Yes |

calamus reads no file, opens no connection and keeps nothing between calls. The host hands in what it holds: the ontology as triples, the table of store names, the caller's values. It depends on `spargebra` and `oxrdf` only, plus `oxttl` if you turn on the `turtle` feature.

## Using it

```toml
[dependencies]
calamus = { git = "https://github.com/repolex-ai/calamus", features = ["turtle"] }
```

Use oxigraph 0.5.6 or later with its `rdf-12` feature, and cargo will give calamus and oxigraph the same `spargebra`. The tests pass on oxigraph 0.5.9 with spargebra 0.4.6 and on oxigraph 0.5.13 with spargebra 0.4.8.

The order is always: parse, bind, names, rewrite, evaluate.

```rust
use calamus::{bind, names::Names, rewrite, Index, Options};
use oxigraph::sparql::SparqlEvaluator;
use spargebra::SparqlParser;

// Once, when the engine starts: the index, from the ontology alone.
let index = Index::from_turtle(&[SUBTEXTURE_TTL, PAN_TTL], &["https://repolex.ai/ontology/subtexture/"])?;
let options = Options { ontology_graph_prefixes: vec!["https://repolex.ai/ontology/".into()], ontology_graphs: vec![] };

// For each query that arrives on the public route or the command line:
let base = names.base();                                  // the store BEING ASKED
let mut query = SparqlParser::new().with_base_iri(&base)?.parse_query(text)?;
bind::bind(&mut query, &declared, &given, &base)?;        // only for a saved query
names.apply(&mut query)?;
let report = rewrite(&mut query, &index, &options)?;
let results = SparqlEvaluator::new().for_query(query).on_store(&store).execute()?;
```

If any step returns an error, refuse the query. Do not run it half-prepared: the result would be an empty answer with no error, which is the fault this library exists to remove.

## rewrite

oxigraph does not reason. If an ontology says `pan:title` is the same as `subtexture:title`, a query for `subtexture:title` finds nothing, and says nothing. `rewrite` fixes the question, not the store: nothing is stored twice, and a change to the ontology takes effect at the next query.

The rules, and there are only these:

- a property is satisfied by itself, by anything declared `rdfs:subPropertyOf` it, and by anything `owl:equivalentProperty` to it, in either direction;
- a class is satisfied by itself, by anything declared `rdfs:subClassOf` it, and by anything `owl:equivalentClass` to it, in either direction.

This is the hierarchy part of OWL 2 QL. It is not the whole profile. `rdfs:domain`, `rdfs:range`, `owl:inverseOf`, `owl:sameAs` and every "there exists some" rule are left out on purpose: in a store that is checked by shapes, they produce answers from malformed data that nobody recorded.

What a rewrite does:

- **One name for one name is a swap in place.** `?x subtexture:title ?t` becomes `?x pan:title ?t`. No extra join.
- **One name for several is asked once per answer.** `?m a pan:Media` becomes `{ SELECT DISTINCT ?m WHERE { VALUES ?v { pan:Image pan:Media pan:Video } ?m a ?v } }`. A thing that is both an Image and a Video is one answer, as it would be if the store reasoned. The DISTINCT covers that one pattern's variables only, so rows your query has twice for its own reasons stay twice. The invented variable stays inside the sub-select and is never a column.
- **In a property path, several names are an alternative** (`p1|p2`), and `!(p)` leaves out every spelling of `p`.
- **Quoted statements are rewritten too**, at any depth: `<<( ?s subtexture:title ?o )>>`.

What it leaves alone:

- a predicate or a class that is a variable (`?s ?p ?o`, `?s a ?c`): you get what is stored, nothing added;
- expressions: `FILTER(?p = subtexture:title)` compares against stored spellings and will most likely match nothing. `Report::left_as_written` lists each such name, so the host can tell its caller;
- anything inside `SERVICE { }`: the store it is sent to rewrites it against its own ontology;
- anything inside `GRAPH <g> { }` for a graph you name as holding an ontology;
- a CONSTRUCT template;
- updates, which calamus does not take.

Two things the host must do:

1. **Build the index from the ontology, never from the data.** An index that depends on what a store holds goes stale the first time a new spelling is written.
2. **Make sure no store holds a statement that uses an abstract name.** The prefixes you pass as abstract are dropped from every answer. If a store ever does hold `subtexture:title`, dropping it hides real data. Assert it in a test.

What it costs, measured by w4r3z-pan on a live Pan store of 84 million statements (2026-11-19, pan 349c745). The question is "count everything that is a `pan:Media`", which fans out to three classes and finds 235,360 things:

| Form | Time |
|---|---|
| `?m a pan:Media`, rewritten by calamus | 0.13 s |
| the same question written by hand with VALUES and DISTINCT | 0.11 s |
| `?m a pan:Image` alone, one class, no fan-out (229,571 things) | 0.07 s |

So the rewrite itself adds about 0.02 s over writing the fan-out by hand. Most of the difference from the one-class query is the fan-out and its DISTINCT, which a correct answer needs either way. A swap of one name for one name adds nothing.

An engine's own working queries should not go through `rewrite`. They use stored spellings and must not change, or slow down, when an ontology gains a name.

To see what an ontology makes of a name:

```
cargo run --features turtle --example targets -- \
  https://repolex.ai/ontology/subtexture/ subtexture.ttl pan.ttl -- \
  https://repolex.ai/ontology/pan/Media
```

## bind

A saved query declares its parameters, one text each: `name[:kind][=default]`. The kinds are `string`, `integer`, `decimal`, `boolean`, `date`, `dateTime` and `iri`.

- No default means the parameter must be given.
- No kind means text, always. Nothing is guessed from how a value looks. A stated kind is checked.
- A value is put into the parsed query in place of the variable. It is never pasted into the query's text, so nothing a caller sends can change what the query asks.
- Every refusal names the parameter.

Bind before rewriting, so a parameter of kind `iri` that is used as a class is rewritten like any other class.

## names

The Subtexture stack writes a thing's address short in documents and queries. `<soul/Note/day-8>` is in the store being asked, `<s3lk13/copia/Moment/x>` is in another store of the same squad, and `<demolishous/tr1pl3x/soul/Note/a>` is in a store of another squad. The first form is ordinary relative resolution against the base of the store being asked. The other two are not, so `Names::apply` moves them after parsing.

Which form an address is, is told by its first part: an installed namespace, a store's name or a squad's name. `Names::check` holds the table to the rules that make that unambiguous. The table is handed in by the host. A Pan running alone hands in a table of one.

## Tests

```
cargo test --features turtle
```

`tests/conformance.rs` runs the rewrite against a real in-memory oxigraph store. An engine that adopts calamus should pass the same cases against a store of its own.

## Looked after by

m4xx, beside Syrinx. The parameter rules and their first tests are w4r3z's.
