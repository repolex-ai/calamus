//! The conformance suite: the rewrite run against a real in-memory oxigraph
//! store. An engine that adopts calamus should pass the same cases against a
//! store of its own (the numbered ones are the cases of the Subtexture spec,
//! `2026_11_18_OWL2_QL_QUERY_REWRITING_SPEC.md`, section 7 and its reviews).

use calamus::{rewrite, Index, Options, Report};
use oxigraph::io::RdfFormat;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use spargebra::{Query, SparqlParser};

/// `base:` is the layer of shared names that is never stored.
const ONTOLOGY: &str = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl:  <http://www.w3.org/2002/07/owl#> .
@prefix base: <http://base.example/> .
@prefix pan:  <http://pan.example/> .

pan:name      owl:equivalentProperty base:name .
pan:relatedTo owl:equivalentProperty base:relatedTo .
pan:title     owl:equivalentProperty base:title .
pan:caption   rdfs:subPropertyOf pan:title .
pan:Media     rdfs:subClassOf base:Thing .
pan:Image     rdfs:subClassOf pan:Media .
pan:Video     rdfs:subClassOf pan:Media .
pan:Media     rdfs:subClassOf [ a owl:Restriction ] .
"#;

const DATA: &str = r#"
@prefix rdf:  <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix pan:  <http://pan.example/> .
@prefix :     <http://data.example/> .

:img1 a pan:Image ; pan:name "one" ; pan:title "t1" ; pan:caption "c1" ; pan:relatedTo :img2 .
:img2 a pan:Image , pan:Video ; pan:name "two" .
:img3 a pan:Image ; pan:title "same" ; pan:caption "same" .
:vid1 a pan:Video ; pan:name "film" ; pan:relatedTo :img1 .
:commit1 a pan:Commit ; pan:name "not a thing" .
_:was rdf:reifies <<( :img1 pan:name "old name" )>> .

GRAPH <http://base.example/ontology/pan> {
  pan:name <http://www.w3.org/2002/07/owl#equivalentProperty> <http://base.example/name> .
}
"#;

const PREFIXES: &str = "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX base: <http://base.example/> PREFIX pan: <http://pan.example/> PREFIX : <http://data.example/> ";

fn index() -> Index {
    Index::from_turtle(&[ONTOLOGY], &["http://base.example/"]).unwrap()
}

fn options() -> Options {
    Options { ontology_graph_prefixes: vec!["http://base.example/ontology/".into()], ontology_graphs: vec![] }
}

fn store() -> Store {
    let store = Store::new().unwrap();
    store.load_from_slice(RdfFormat::TriG, DATA.as_bytes()).unwrap();
    store
}

fn parse(sparql: &str) -> Query {
    SparqlParser::new().parse_query(&format!("{PREFIXES}{sparql}")).unwrap()
}

fn rewritten(sparql: &str) -> (Query, Report) {
    let mut query = parse(sparql);
    let report = rewrite(&mut query, &index(), &options()).unwrap();
    (query, report)
}

/// Columns, and rows as sorted text, of a parsed query handed to oxigraph
/// as it is, with no print and re-parse.
fn answer(query: Query) -> (Vec<String>, Vec<String>) {
    match SparqlEvaluator::new().for_query(query).on_store(&store()).execute().unwrap() {
        QueryResults::Solutions(solutions) => {
            let columns: Vec<String> = solutions.variables().iter().map(|v| v.as_str().to_string()).collect();
            let mut rows: Vec<String> = solutions
                .map(|s| {
                    let s = s.unwrap();
                    columns.iter().map(|c| s.get(c.as_str()).map(|t| t.to_string().replace("http://data.example/", ":").replace("http://pan.example/", "pan:")).unwrap_or_default()).collect::<Vec<_>>().join(" ")
                })
                .collect();
            rows.sort();
            (columns, rows)
        }
        QueryResults::Boolean(yes) => (vec![], vec![yes.to_string()]),
        QueryResults::Graph(triples) => {
            let mut rows: Vec<String> = triples.map(|t| t.unwrap().to_string().replace("http://data.example/", ":")).collect();
            rows.sort();
            (vec![], rows)
        }
    }
}

/// Rows of a query after the rewrite.
fn rows(sparql: &str) -> Vec<String> {
    answer(rewritten(sparql).0).1
}

/// Rows of the same query with no rewrite: what the store holds, as asked.
fn stored(sparql: &str) -> Vec<String> {
    answer(parse(sparql)).1
}

#[test]
fn case_1_a_shared_property_with_one_bridge_is_swapped_in_place() {
    let (query, report) = rewritten("SELECT ?x ?n WHERE { ?x base:name ?n }");
    assert_eq!(report, Report { swapped: 1, fanned_out: 0 });
    // A swap adds nothing: the query is the one a caller would have written.
    assert_eq!(query, parse("SELECT ?x ?n WHERE { ?x pan:name ?n }"));
    assert_eq!(answer(query).1.len(), 4);
    assert!(stored("SELECT ?x ?n WHERE { ?x base:name ?n }").is_empty());
}

#[test]
fn case_2_a_shared_property_in_a_path_walks_the_stored_edges() {
    assert_eq!(rows("SELECT ?b WHERE { :vid1 base:relatedTo+ ?b }"), ["<:img1>", "<:img2>"]);
}

#[test]
fn case_3_a_shared_property_nothing_bridges_to_is_a_clean_empty_match() {
    let (query, report) = rewritten("SELECT ?x ?a WHERE { ?x base:abstract ?a }");
    assert!(!report.changed());
    assert!(answer(query).1.is_empty());
    // And inside OPTIONAL it costs the outer rows nothing.
    assert_eq!(rows("SELECT ?x ?a WHERE { ?x a pan:Video OPTIONAL { ?x base:abstract ?a } }").len(), 2);
}

#[test]
fn case_4_the_root_class_finds_every_thing_and_nothing_else() {
    assert_eq!(rows("SELECT ?x WHERE { ?x a base:Thing }"), ["<:img1>", "<:img2>", "<:img3>", "<:vid1>"]);
}

#[test]
fn case_5_a_class_in_the_middle_finds_its_subclasses() {
    assert_eq!(rows("SELECT ?m WHERE { ?m a pan:Media }"), ["<:img1>", "<:img2>", "<:img3>", "<:vid1>"]);
    assert!(stored("SELECT ?m WHERE { ?m a pan:Media }").is_empty());
    // A class with nothing under it is asked for as written.
    assert_eq!(rewritten("SELECT ?m WHERE { ?m a pan:Video }").0, parse("SELECT ?m WHERE { ?m a pan:Video }"));
}

#[test]
fn case_6_an_unbound_property_or_class_returns_only_what_is_stored() {
    for sparql in ["SELECT ?p ?o WHERE { :img1 ?p ?o }", "SELECT ?c (COUNT(?s) AS ?n) WHERE { ?s a ?c } GROUP BY ?c", "SELECT ?x WHERE { ?x ?p ?o FILTER(?p = base:name) }"] {
        let (query, report) = rewritten(sparql);
        assert!(!report.changed(), "{sparql}");
        assert_eq!(answer(query), answer(parse(sparql)), "{sparql}");
    }
    assert!(!rows("SELECT ?p ?o WHERE { :img1 ?p ?o }").iter().any(|r| r.contains("base.example")));
}

#[test]
fn case_7_a_service_block_is_passed_on_as_written() {
    let sparql = "SELECT ?x ?n WHERE { SERVICE <http://other.example/sparql> { ?x base:name ?n . ?x a base:Thing } }";
    let (query, report) = rewritten(sparql);
    assert!(!report.changed());
    assert_eq!(query, parse(sparql));
}

#[test]
fn case_8_an_ontology_graph_is_read_as_written() {
    let sparql = "SELECT ?p WHERE { GRAPH <http://base.example/ontology/pan> { ?p owl:equivalentProperty base:name } }";
    assert_eq!(rewritten(sparql).0, parse(sparql));
    assert_eq!(rows(sparql), ["<pan:name>"]);
    // Any other named graph is data and is rewritten.
    assert!(rewritten("SELECT ?x WHERE { GRAPH <http://data.example/g> { ?x base:name ?n } }").1.changed());
}

#[test]
fn case_9_a_shared_name_inside_a_quoted_statement_is_rewritten() {
    assert_eq!(rows("SELECT ?s ?o WHERE { ?r rdf:reifies <<( ?s base:name ?o )>> }"), ["<:img1> \"old name\""]);
    assert!(stored("SELECT ?s ?o WHERE { ?r rdf:reifies <<( ?s base:name ?o )>> }").is_empty());
}

#[test]
fn case_10_nothing_invented_for_a_fan_out_is_in_the_answer() {
    let (columns, rows) = answer(rewritten("SELECT * WHERE { ?m a pan:Media }").0);
    assert_eq!(columns, ["m"]);
    assert_eq!(rows.len(), 4);
    // The columns are the ones the caller's own query has, in its order.
    let both = "SELECT * WHERE { ?x base:title ?t . ?x a pan:Media }";
    assert_eq!(answer(rewritten(both).0).0, answer(parse(both)).0);
}

#[test]
fn a_thing_with_two_spellings_or_two_kinds_is_one_answer() {
    // :img2 is an Image and a Video: one Media, not two.
    assert_eq!(rows("SELECT (COUNT(?m) AS ?n) WHERE { ?m a pan:Media }"), ["\"4\"^^<http://www.w3.org/2001/XMLSchema#integer>"]);
    // :img3 has the same text as its title and as its caption: one row.
    assert_eq!(rows("SELECT ?x WHERE { ?x base:title \"same\" }"), ["<:img3>"]);
    // Different values under two spellings are different answers.
    assert_eq!(rows("SELECT ?t WHERE { :img1 base:title ?t }"), ["\"c1\"", "\"t1\""]);
    // Rows the caller's own query has twice stay twice.
    assert_eq!(rows("SELECT ?m WHERE { { ?m a pan:Media } UNION { ?m a pan:Media } }").len(), 8);
}

#[test]
fn a_fan_out_in_a_path_is_an_alternative_and_gives_each_answer_once() {
    let (query, report) = rewritten("SELECT ?x WHERE { ?x base:title|pan:name \"same\" }");
    assert_eq!(report, Report { swapped: 0, fanned_out: 1 });
    // :img3 matches under two spellings and is one answer.
    assert_eq!(answer(query).1, ["<:img3>"]);
    // A stored property with a sub-property asks for it too.
    assert_eq!(rows("SELECT ?t WHERE { :img1 pan:title ?t }"), ["\"c1\"", "\"t1\""]);
    assert_eq!(rows("SELECT ?x ?t WHERE { ?x ^pan:relatedTo/base:title ?t }"), ["<:img2> \"c1\"", "<:img2> \"t1\""]);
}

#[test]
fn any_property_but_a_shared_one_leaves_out_every_spelling() {
    let seen = rows("SELECT DISTINCT ?o WHERE { :img1 !(base:title|rdf:type) ?o }");
    assert_eq!(seen, ["\"one\"", "<:img2>"]);
}

#[test]
fn a_blank_node_keeps_joining_across_a_fanned_out_pattern() {
    // The same unnamed thing must be a Video and carry the title.
    assert!(rows("SELECT ?t WHERE { [] a pan:Video ; base:title ?t }").is_empty());
    assert_eq!(rows("SELECT ?t WHERE { [] pan:name \"one\" ; base:title ?t }"), ["\"c1\"", "\"t1\""]);
    assert_eq!(rows("SELECT ?n WHERE { [] a pan:Media ; pan:name ?n }"), ["\"film\"", "\"one\"", "\"two\""]);
    // And it is still not a column.
    assert_eq!(answer(rewritten("SELECT * WHERE { [] a pan:Media ; pan:name ?n }").0).0, ["n"]);
}

#[test]
fn a_pattern_with_no_variables_is_a_yes_or_no_and_multiplies_nothing() {
    assert_eq!(rows("ASK { :img3 base:title \"same\" }"), ["true"]);
    assert_eq!(rows("ASK { :vid1 base:title \"same\" }"), ["false"]);
    assert_eq!(rows("ASK { :img2 a pan:Media }"), ["true"]);
    // :img3 matches under two spellings; the two videos are still two rows.
    assert_eq!(rows("SELECT ?v WHERE { :img3 base:title \"same\" . ?v a pan:Video }").len(), 2);
}

#[test]
fn exists_is_rewritten_and_a_construct_template_is_not() {
    assert_eq!(rows("SELECT ?x WHERE { ?x a pan:Video FILTER EXISTS { ?x base:name \"film\" } }"), ["<:vid1>"]);
    assert_eq!(rows("SELECT ?x WHERE { ?x a pan:Video FILTER NOT EXISTS { ?x a pan:Media . ?x base:relatedTo ?y } }"), ["<:img2>"]);
    let made = rows("CONSTRUCT { ?x base:name ?n } WHERE { ?x base:name ?n . ?x a pan:Video }");
    assert_eq!(made.len(), 2);
    assert!(made.iter().all(|t| t.contains("<http://base.example/name>")));
}

#[test]
fn an_invented_variable_never_meets_one_the_caller_wrote() {
    // The caller already uses the name calamus would pick first.
    let (columns, rows) = answer(rewritten("SELECT ?_ql_0 ?m WHERE { ?m a pan:Media . ?m pan:name ?_ql_0 }").0);
    assert_eq!(columns, ["_ql_0", "m"]);
    assert_eq!(rows, ["\"film\" <:vid1>", "\"one\" <:img1>", "\"two\" <:img2>"]);
}

#[test]
fn the_rewritten_query_can_be_printed_and_read_back() {
    // An engine that tells its caller what was run prints the query. The
    // text must be SPARQL and must ask the same thing.
    for sparql in [
        "SELECT * WHERE { ?m a pan:Media }",
        "SELECT ?x ?t WHERE { ?x base:title ?t ; a base:Thing OPTIONAL { ?x base:name ?n } }",
        "ASK { :img3 base:title \"same\" }",
        "SELECT ?t WHERE { [] pan:name \"one\" ; base:title ?t }",
        "SELECT ?s ?o WHERE { ?r rdf:reifies <<( ?s base:name ?o )>> }",
        "SELECT ?b WHERE { :vid1 base:relatedTo+/base:title ?b }",
        "SELECT (COUNT(?m) AS ?n) WHERE { ?m a pan:Media } ",
    ] {
        let (query, _) = rewritten(sparql);
        let read_back = SparqlParser::new().parse_query(&query.to_string()).unwrap_or_else(|e| panic!("{sparql}\n{query}\n{e}"));
        assert_eq!(answer(read_back), answer(query), "{sparql}");
    }
}

#[test]
fn an_empty_index_changes_nothing() {
    let mut query = parse("SELECT ?x WHERE { ?x base:name ?n . ?x a base:Thing }");
    let report = rewrite(&mut query, &Index::default(), &options()).unwrap();
    assert!(!report.changed());
    assert_eq!(query, parse("SELECT ?x WHERE { ?x base:name ?n . ?x a base:Thing }"));
}

#[test]
fn bind_then_names_then_rewrite_is_the_whole_preparation() {
    use calamus::bind::{bind, declarations};
    use calamus::names::{Kind, Names, Store};

    let names = Names {
        root: "http://data.example/".into(),
        squad: "squad".into(),
        user: "mine".into(),
        namespaces: vec!["pan".into()],
        stores: ["mine", "theirs"].iter().map(|n| Store { squad: "squad".into(), name: n.to_string(), id: String::new(), kind: Kind::Personal }).collect(),
    };
    names.check().unwrap();
    // A saved query: the kind of thing to list, and what it must be related to.
    let saved = "PREFIX base: <http://base.example/> SELECT ?x WHERE { ?x a ?kind . ?x base:relatedTo ?to }";
    let declared = declarations(&["kind:iri".into(), "to:iri".into()], &names.base()).unwrap();
    let given = vec![("kind".to_string(), "<http://pan.example/Media>".to_string()), ("to".to_string(), "theirs/pan/Image/x".to_string())];

    let mut query = SparqlParser::new().with_base_iri(names.base()).unwrap().parse_query(saved).unwrap();
    bind(&mut query, &declared, &given, &names.base()).unwrap();
    assert_eq!(names.apply(&mut query).unwrap(), 1);
    let report = rewrite(&mut query, &index(), &options()).unwrap();

    // The class came in as a value and was still fanned out; the property
    // was swapped; the short address points at the other store.
    assert_eq!(report, Report { swapped: 1, fanned_out: 1 });
    let text = query.to_string();
    assert!(text.contains("<http://pan.example/relatedTo> <http://data.example/squad/theirs/pan/Image/x>"), "{text}");
    assert!(text.contains("<http://pan.example/Image>") && text.contains("<http://pan.example/Video>"), "{text}");
}
