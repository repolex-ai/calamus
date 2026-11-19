//! Print what an ontology makes of some names.
//!
//!   cargo run --features turtle --example targets -- <abstract-prefix> <file.ttl>... -- <name>...
use calamus::{Index, Targets};
use oxrdf::NamedNode;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let split = args.iter().position(|a| a == "--").expect("usage: <abstract-prefix> <file.ttl>... -- <name>...");
    let texts: Vec<String> = args[1..split].iter().map(|p| std::fs::read_to_string(p).expect(p)).collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    let index = Index::from_turtle(&texts, &[args[0].as_str()]).expect("ontology");
    let (properties, classes) = index.len();
    println!("{properties} properties and {classes} classes are asked for under other names");
    for name in &args[split + 1..] {
        let node = NamedNode::new(name).expect("a full address");
        for (kind, found) in [("property", index.property(node.as_ref())), ("class", index.class(node.as_ref()))] {
            match found {
                Targets::Same => {}
                Targets::One(one) => println!("{kind} {name}\n  -> {one}"),
                Targets::Many(many) => println!("{kind} {name}\n{}", many.iter().map(|m| format!("  -> {m}")).collect::<Vec<_>>().join("\n")),
            }
        }
    }
}
