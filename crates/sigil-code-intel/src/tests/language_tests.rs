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
fn rust_document_symbols_honors_zero_limit() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("lib.rs");
    fs::write(&path, "pub fn hello() {}\npub fn world() {}\n").expect("source should write");

    let symbols = rust_document_symbols(temp.path(), &path, None, 0).expect("symbols should parse");

    assert!(symbols.is_empty());
}

#[test]
fn rust_document_symbols_stops_after_reaching_limit() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("lib.rs");
    fs::write(
        &path,
        "pub fn first() {}\npub fn second() {}\npub fn third() {}\n",
    )
    .expect("source should write");

    let symbols = rust_document_symbols(temp.path(), &path, None, 1).expect("symbols should parse");

    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "first");
}

#[test]
fn rust_document_symbols_skips_incomplete_named_items() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("lib.rs");
    fs::write(&path, "fn () {}\nstruct ;\npub fn valid() {}\n").expect("source should write");

    let symbols =
        rust_document_symbols(temp.path(), &path, None, 10).expect("symbols should parse");

    assert!(symbols.iter().any(|symbol| symbol.name == "valid"));
}

#[test]
fn rust_document_symbols_extracts_all_supported_symbol_kinds_and_ranges() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("lib.rs");
    fs::write(
        &path,
        r#"
pub mod module_name {}
pub enum Choice { A }
pub trait Runnable {}
pub type Alias = i32;
pub const LIMIT: usize = 1;
pub static FLAG: bool = true;
impl Runnable for Choice {}
"#,
    )
    .expect("source should write");

    let symbols =
        rust_document_symbols(temp.path(), &path, None, 20).expect("symbols should parse");
    let names_and_kinds = symbols
        .iter()
        .map(|symbol| (symbol.name.as_str(), symbol.kind.as_str()))
        .collect::<Vec<_>>();

    for expected in [
        ("module_name", "module"),
        ("Choice", "enum"),
        ("Runnable", "trait"),
        ("Alias", "type"),
        ("LIMIT", "const"),
        ("FLAG", "static"),
        ("Runnable", "impl"),
    ] {
        assert!(names_and_kinds.contains(&expected), "missing {expected:?}");
    }
    assert!(symbols.iter().all(|symbol| symbol.range.start_line > 0));
}

#[test]
fn rust_document_symbols_returns_empty_for_query_miss() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("lib.rs");
    fs::write(&path, "pub fn visible() {}\n").expect("source should write");

    let symbols = rust_document_symbols(temp.path(), &path, Some("absent"), 10)
        .expect("symbols should parse");

    assert!(symbols.is_empty());
}

#[test]
fn rust_document_symbols_reports_read_errors_with_path_context() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("missing.rs");

    let error = rust_document_symbols(temp.path(), &path, None, 10)
        .expect_err("missing source should fail");

    assert!(format!("{error:#}").contains("failed to read"));
    assert!(format!("{error:#}").contains("missing.rs"));
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

#[test]
fn rust_syntax_diagnostics_reports_read_errors_with_path_context() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("missing.rs");

    let error =
        rust_syntax_diagnostics(temp.path(), &path).expect_err("missing source should fail");

    assert!(format!("{error:#}").contains("failed to read"));
    assert!(format!("{error:#}").contains("missing.rs"));
}

#[test]
fn rust_syntax_diagnostics_reports_missing_nodes() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("missing.rs");

    let mut diagnostics = Vec::new();
    for source in [
        "fn () {}\n",
        "struct ;\n",
        "enum {}\n",
        "fn missing(param: ) {}\n",
        "const : i32 = 1;\n",
    ] {
        fs::write(&path, source).expect("source should write");
        diagnostics
            .extend(rust_syntax_diagnostics(temp.path(), &path).expect("diagnostics should parse"));
    }

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.starts_with("missing "))
    );
}
