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
fn repository_context_ids_use_portable_path_separators() {
    let hit = CodeContextBuilder::new().repo_file_hit(
        std::path::Path::new("crates")
            .join("sigil-runtime")
            .join("src/lib.rs"),
        "runtime context",
    );

    assert_eq!(hit.item.id, "repo-file:crates/sigil-runtime/src/lib.rs");
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
            max_source_files: 16,
            max_index_bytes_per_file: 1024,
            ..RepoMapLiteOptions::default()
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
            && symbol.language == "rust"
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
            max_source_files: 16,
            max_index_bytes_per_file: 24,
            ..RepoMapLiteOptions::default()
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
    assert!(file.truncated);
}

#[test]
fn context_repo_map_lite_does_not_charge_unsupported_files_to_source_budget() {
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
            max_source_files: 1,
            max_index_bytes_per_file: 1024,
            ..RepoMapLiteOptions::default()
        },
    )
    .expect("repo map should build");

    assert_eq!(map.files_scanned, 1);
    assert_eq!(map.source_files.len(), 1);
    assert!(
        map.symbols
            .iter()
            .any(|symbol| symbol.name == "should_not_be_scanned_after_cap")
    );
}

#[test]
fn context_repo_map_lite_builds_multilingual_unique_reference_edges() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixtures = [
        (
            "rust/lib.rs",
            "pub fn rust_target() {}\nfn caller() { rust_target(); }\n",
        ),
        (
            "python/main.py",
            "def python_target():\n    pass\n\ndef caller():\n    python_target()\n",
        ),
        (
            "web/main.ts",
            "function ts_target(): void {}\nfunction caller(): void { ts_target(); }\n",
        ),
        (
            "cmd/main.go",
            "package main\nfunc goTarget() {}\nfunc caller() { goTarget() }\n",
        ),
    ];
    for (relative, source) in fixtures {
        let path = temp.path().join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture dir");
        fs::write(path, source).expect("write fixture");
    }

    let map = build_repo_map_lite(temp.path(), RepoMapLiteOptions::default())
        .expect("multilingual repo map");
    let languages = map
        .source_files
        .iter()
        .map(|file| file.language.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        languages,
        std::collections::BTreeSet::from(["go", "python", "rust", "typescript"])
    );
    for expected in ["rust_target", "python_target", "ts_target", "goTarget"] {
        assert!(
            map.symbols.iter().any(|symbol| symbol.name == expected),
            "missing definition {expected}: {:?}",
            map.symbols
        );
        assert!(
            map.references
                .iter()
                .any(|reference| reference.name == expected),
            "missing reference {expected}: {:?}",
            map.references
        );
        assert!(
            map.edges.iter().any(|edge| {
                edge.kind == RepoMapEdgeKind::References
                    && edge.to.contains(&format!(":{expected}:"))
            }),
            "missing unique reference edge {expected}: {:?}",
            map.edges
        );
    }
}

#[test]
fn context_repo_map_lite_omits_ambiguous_unresolved_and_cross_language_edges() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixtures = [
        ("python/a.py", "def shared():\n    pass\n"),
        ("python/b.py", "def shared():\n    pass\n"),
        (
            "python/c.py",
            "def caller():\n    shared()\n    missing()\n",
        ),
        ("python/foreign.py", "def foreign():\n    pass\n"),
        ("web/caller.js", "function caller() { foreign(); }\n"),
    ];
    for (relative, source) in fixtures {
        let path = temp.path().join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture dir");
        fs::write(path, source).expect("write fixture");
    }

    let map = build_repo_map_lite(temp.path(), RepoMapLiteOptions::default())
        .expect("ambiguous repo map");
    let reference_edges = map
        .edges
        .iter()
        .filter(|edge| edge.kind == RepoMapEdgeKind::References)
        .collect::<Vec<_>>();

    assert!(
        reference_edges
            .iter()
            .all(|edge| !edge.to.contains(":shared:"))
    );
    assert!(
        reference_edges
            .iter()
            .all(|edge| !edge.to.contains(":missing:"))
    );
    assert!(
        reference_edges
            .iter()
            .all(|edge| !edge.to.contains(":foreign:"))
    );
}

#[test]
fn context_repo_map_lite_respects_ignore_secret_symlink_and_walk_caps() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join(".gitignore"), "ignored.py\n").expect("write gitignore");
    fs::write(temp.path().join("a.py"), "def visible():\n    pass\n").expect("write visible");
    fs::write(temp.path().join("ignored.py"), "def ignored():\n    pass\n").expect("write ignored");
    fs::write(temp.path().join(".env.py"), "def secret():\n    pass\n").expect("write secret");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let outside = tempfile::NamedTempFile::new().expect("outside file");
        fs::write(outside.path(), "def escaped():\n    pass\n").expect("write outside");
        symlink(outside.path(), temp.path().join("linked.py")).expect("create symlink");

        let map = build_repo_map_lite(temp.path(), RepoMapLiteOptions::default())
            .expect("filtered repo map");
        assert_eq!(
            map.source_files
                .iter()
                .map(|file| file.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("a.py")]
        );
    }

    let capped = build_repo_map_lite(
        temp.path(),
        RepoMapLiteOptions {
            max_walked_entries: 2,
            max_source_files: 8,
            ..RepoMapLiteOptions::default()
        },
    )
    .expect("walk-capped repo map");
    assert_eq!(capped.entries_walked, 2);
    assert!(capped.source_files.len() <= 1);
}

#[test]
fn context_repo_map_lite_applies_global_caps_and_is_deterministic() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(
        temp.path().join("a.js"),
        "function alpha() {}\nfunction beta() {}\nalpha();\nbeta();\n",
    )
    .expect("write javascript");
    let options = RepoMapLiteOptions {
        max_definitions: 1,
        max_references: 1,
        max_edges: 1,
        ..RepoMapLiteOptions::default()
    };

    let first = build_repo_map_lite(temp.path(), options).expect("first repo map");
    let second = build_repo_map_lite(temp.path(), options).expect("second repo map");

    assert_eq!(first, second);
    assert_eq!(first.symbols.len(), 1);
    assert_eq!(first.references.len(), 1);
    assert_eq!(first.edges.len(), 1);
}
