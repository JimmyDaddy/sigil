use std::fs;

use sigil_kernel::{ContextInclusionReason, ContextSensitivity, ContextSource, ContextTrustLevel};

use super::*;

fn range() -> CodeRange {
    CodeRange {
        start_line: 10,
        start_character: 2,
        end_line: 10,
        end_character: 12,
    }
}

#[test]
fn context_code_symbol_diagnostic_and_reference_hits_keep_lsp_provenance() {
    let builder = CodeContextBuilder::new().source_event_id("event-code-context");
    let symbol = CodeSymbol {
        name: "parse_config".to_owned(),
        kind: "function".to_owned(),
        path: "src/config.rs".to_owned(),
        range: range(),
        container_name: Some("config".to_owned()),
    };
    let diagnostic = CodeDiagnostic {
        path: "src/config.rs".to_owned(),
        range: range(),
        severity: "warning".to_owned(),
        message: "unused result".to_owned(),
        source: Some("rust-analyzer".to_owned()),
    };
    let reference = CodeLocation {
        path: "src/main.rs".to_owned(),
        range: range(),
        preview: Some("parse_config()".to_owned()),
    };

    let symbol_hit = builder.symbol_hit(&symbol);
    let diagnostic_hit = builder.diagnostic_hit(&diagnostic);
    let reference_hit = builder.reference_hit(&reference);

    assert_eq!(symbol_hit.item.source, ContextSource::LspSymbol);
    assert_eq!(diagnostic_hit.item.source, ContextSource::LspDiagnostic);
    assert_eq!(reference_hit.item.source, ContextSource::LspReference);
    assert_eq!(
        symbol_hit.item.source_event_id.as_deref(),
        Some("event-code-context")
    );
    assert_eq!(
        symbol_hit.item.trust_level,
        ContextTrustLevel::UntrustedRepositoryData
    );
    assert_eq!(
        diagnostic_hit.item.sensitivity,
        ContextSensitivity::Repository
    );
    assert_eq!(
        reference_hit.item.inclusion_reason,
        ContextInclusionReason::RetrievalHit
    );
    assert!(symbol_hit.snippet.contains("parse_config"));
    assert!(diagnostic_hit.snippet.contains("unused result"));
    assert!(reference_hit.snippet.contains("parse_config()"));
    symbol_hit
        .item
        .validate()
        .expect("symbol context item is valid");
    diagnostic_hit
        .item
        .validate()
        .expect("diagnostic context item is valid");
    reference_hit
        .item
        .validate()
        .expect("reference context item is valid");
}

#[test]
fn context_repo_file_and_diff_hits_apply_secret_egress_filtering() {
    let blocked_secret = CodeContextBuilder::new()
        .sensitivity(ContextSensitivity::Secret)
        .repo_file_hit(".env", "OPENAI_API_KEY=secret");
    let approved_secret = CodeContextBuilder::new()
        .sensitivity(ContextSensitivity::Secret)
        .egress_decision("egress-approved-1")
        .repo_file_hit(".env", "OPENAI_API_KEY=secret");
    let diff = CodeContextBuilder::new().current_diff_hit("src/lib.rs", "+fn new_api() {}");

    assert_eq!(blocked_secret.item.source, ContextSource::RepositoryFile);
    assert_eq!(
        blocked_secret.item.inclusion_reason,
        ContextInclusionReason::ExcludedSecret
    );
    blocked_secret
        .item
        .validate()
        .expect("excluded secret can be represented");

    assert_eq!(
        approved_secret.item.inclusion_reason,
        ContextInclusionReason::RetrievalHit
    );
    assert_eq!(
        approved_secret.item.egress_decision.as_deref(),
        Some("egress-approved-1")
    );
    approved_secret
        .item
        .validate()
        .expect("approved secret has egress decision");

    assert_eq!(diff.item.source, ContextSource::CurrentDiff);
    assert_eq!(
        diff.item.inclusion_reason,
        ContextInclusionReason::RetrievalHit
    );
    assert!(diff.snippet.contains("+fn new_api"));
}

#[test]
fn context_repo_map_lite_builds_rust_source_files_symbols_and_edges() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("crates/sigil-runtime/src");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::write(
        source_dir.join("context.rs"),
        "pub struct RuntimeContextCandidates;\npub fn build_context() {}\n",
    )
    .expect("write source");

    let map = build_repo_map_lite(
        temp.path(),
        RepoMapLiteOptions {
            max_files_scanned: 16,
            max_index_bytes_per_file: 1024,
        },
    )
    .expect("repo map should build");

    assert_eq!(map.files_scanned, 1);
    assert!(map.source_files.iter().any(|file| {
        file.path == std::path::Path::new("crates/sigil-runtime/src/context.rs")
            && file.language == "rust"
            && file.indexed_text.contains("RuntimeContextCandidates")
    }));
    assert!(map.symbols.iter().any(|symbol| {
        symbol.name == "RuntimeContextCandidates"
            && symbol.kind == RepoSymbolKind::Struct
            && symbol.path == std::path::Path::new("crates/sigil-runtime/src/context.rs")
            && symbol.range.is_some()
    }));
    assert!(map.symbols.iter().any(|symbol| {
        symbol.name == "build_context" && symbol.kind == RepoSymbolKind::Function
    }));
    assert!(map.edges.iter().any(|edge| {
        edge.kind == RepoMapEdgeKind::DeclaredIn
            && edge.from.contains("RuntimeContextCandidates")
            && edge.to == "file:crates/sigil-runtime/src/context.rs"
    }));
}

#[test]
fn context_repo_map_lite_skips_local_generated_and_secret_like_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join("crates/sigil-runtime/src")).expect("create source dir");
    fs::write(
        temp.path().join("crates/sigil-runtime/src/context.rs"),
        "pub fn visible_context() {}\n",
    )
    .expect("write visible source");
    fs::create_dir_all(temp.path().join("crates/.repo-local-dev/src"))
        .expect("create repo-local dir");
    fs::write(
        temp.path().join("crates/.repo-local-dev/src/hidden.rs"),
        "pub fn hidden_context() {}\n",
    )
    .expect("write repo-local source");
    fs::create_dir_all(temp.path().join("crates/target/debug")).expect("create target dir");
    fs::write(
        temp.path().join("crates/target/debug/generated.rs"),
        "pub fn generated_context() {}\n",
    )
    .expect("write generated source");
    fs::create_dir_all(temp.path().join("crates/secret/src")).expect("create secret dir");
    fs::write(
        temp.path().join("crates/secret/src/private_key.rs"),
        "pub fn secret_context() {}\n",
    )
    .expect("write secret source");

    let map = build_repo_map_lite(temp.path(), RepoMapLiteOptions::default())
        .expect("repo map should build");
    let paths = map
        .source_files
        .iter()
        .map(|file| file.path.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    assert_eq!(paths, vec!["crates/sigil-runtime/src/context.rs"]);
    assert!(map.symbols.iter().all(|symbol| {
        symbol.name != "hidden_context"
            && symbol.name != "generated_context"
            && symbol.name != "secret_context"
    }));
}

#[test]
fn context_repo_map_lite_caps_index_text_per_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("crates/sigil-runtime/src");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::write(
        source_dir.join("large.rs"),
        format!("pub fn marker() {{}}\n{}", "a".repeat(200)),
    )
    .expect("write source");

    let map = build_repo_map_lite(
        temp.path(),
        RepoMapLiteOptions {
            max_files_scanned: 16,
            max_index_bytes_per_file: 24,
        },
    )
    .expect("repo map should build");
    let file = map
        .source_files
        .iter()
        .find(|file| file.path == std::path::Path::new("crates/sigil-runtime/src/large.rs"))
        .expect("large source file should be indexed");

    assert!(file.indexed_text.len() <= 24);
    assert!(file.indexed_text.contains("pub fn marker"));
}

#[test]
fn context_repo_map_lite_applies_scan_cap_before_rust_filter() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join("crates/sigil-runtime/src")).expect("create source dir");
    fs::write(temp.path().join("crates/README.md"), "non-rust context\n")
        .expect("write non-rust file");
    fs::write(
        temp.path().join("crates/sigil-runtime/src/context.rs"),
        "pub fn should_not_be_scanned_after_cap() {}\n",
    )
    .expect("write rust file");

    let map = build_repo_map_lite(
        temp.path(),
        RepoMapLiteOptions {
            max_files_scanned: 1,
            max_index_bytes_per_file: 1024,
        },
    )
    .expect("repo map should build");

    assert_eq!(map.files_scanned, 1);
    assert!(map.source_files.is_empty());
    assert!(map.symbols.is_empty());
}
