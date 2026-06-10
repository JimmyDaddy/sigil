use std::fs;

use super::*;

#[test]
fn rust_document_symbols_extracts_named_items() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("lib.rs");
    fs::write(
        &path,
        r#"
pub struct AppState;

impl AppState {
    pub fn run(&self) {}
}

fn helper() {}
"#,
    )
    .expect("source should write");

    let symbols =
        rust_document_symbols(temp.path(), &path, Some("app"), 10).expect("symbols should parse");

    assert!(symbols.iter().any(|symbol| symbol.name == "AppState"));
    assert!(symbols.iter().all(|symbol| symbol.path == "lib.rs"));
}

#[test]
fn rust_syntax_diagnostics_reports_parse_errors() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("broken.rs");
    fs::write(&path, "fn broken( {").expect("source should write");

    let diagnostics =
        rust_syntax_diagnostics(temp.path(), &path).expect("diagnostics should parse");

    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == "error" && diagnostic.message.contains("syntax")
    }));
}
