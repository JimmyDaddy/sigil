use std::fs;

use serde_json::json;

use super::*;
use crate::workspace::file_uri_from_path;

#[test]
fn apply_text_edits_handles_utf16_offsets_and_descending_application() {
    let current = "fn hello() {\n    let face = \"😀\";\n}\n";
    let edits = vec![
        CodeTextEdit {
            range: CodeRange {
                start_line: 1,
                start_character: 3,
                end_line: 1,
                end_character: 8,
            },
            new_text: "greet".to_owned(),
        },
        CodeTextEdit {
            range: CodeRange {
                start_line: 2,
                start_character: 16,
                end_line: 2,
                end_character: 18,
            },
            new_text: "🙂".to_owned(),
        },
    ];

    let updated = apply_text_edits(current, &edits).expect("edits should apply");

    assert_eq!(updated, "fn greet() {\n    let face = \"🙂\";\n}\n");
}

#[test]
fn apply_text_edits_rejects_overlapping_ranges() {
    let current = "abcdef\n";
    let edits = vec![
        CodeTextEdit {
            range: CodeRange {
                start_line: 1,
                start_character: 1,
                end_line: 1,
                end_character: 4,
            },
            new_text: "x".to_owned(),
        },
        CodeTextEdit {
            range: CodeRange {
                start_line: 1,
                start_character: 3,
                end_line: 1,
                end_character: 5,
            },
            new_text: "y".to_owned(),
        },
    ];

    let error = apply_text_edits(current, &edits).expect_err("overlap should fail");

    assert!(error.to_string().contains("overlapping"));
}

#[test]
fn workspace_edit_from_lsp_filters_external_and_resource_changes() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let inside = temp.path().join("lib.rs");
    fs::write(&inside, "fn hello() {}\n").expect("source should write");
    let outside = tempfile::NamedTempFile::new().expect("outside file should build");
    let edit = json!({
        "changes": {
            file_uri_from_path(&inside): [{
                "range": {
                    "start": { "line": 0, "character": 3 },
                    "end": { "line": 0, "character": 8 }
                },
                "newText": "greet"
            }],
            file_uri_from_path(outside.path()): [{
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 1 }
                },
                "newText": "x"
            }]
        },
        "documentChanges": [
            { "kind": "rename", "oldUri": "file:///tmp/a", "newUri": "file:///tmp/b" }
        ]
    });

    let parsed = workspace_edit_from_lsp(temp.path(), &edit).expect("edit should parse");

    assert_eq!(parsed.files.len(), 1);
    assert_eq!(parsed.files[0].path, "lib.rs");
    assert_eq!(parsed.external_changes_filtered, 1);
    assert_eq!(parsed.unsupported_changes_filtered, 1);
}

#[test]
fn workspace_edit_from_lsp_accepts_document_changes_edits() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let inside = temp.path().join("lib.rs");
    fs::write(&inside, "fn hello() {}\n").expect("source should write");
    let edit = json!({
        "documentChanges": [{
            "textDocument": { "uri": file_uri_from_path(&inside), "version": 1 },
            "edits": [{
                "range": {
                    "start": { "line": 0, "character": 3 },
                    "end": { "line": 0, "character": 8 }
                },
                "newText": "greet"
            }]
        }]
    });

    let parsed = workspace_edit_from_lsp(temp.path(), &edit).expect("edit should parse");

    assert_eq!(parsed.files.len(), 1);
    assert_eq!(parsed.files[0].edits[0].new_text, "greet");
}

#[test]
fn workspace_edit_preview_matches_materialized_text_edits() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("lib.rs");
    fs::write(&path, "fn hello() {}\n").expect("source should write");
    let edit = CodeWorkspaceEdit {
        files: vec![CodeEditFile {
            path: "lib.rs".to_owned(),
            edits: vec![CodeTextEdit {
                range: CodeRange {
                    start_line: 1,
                    start_character: 3,
                    end_line: 1,
                    end_character: 8,
                },
                new_text: "greet".to_owned(),
            }],
        }],
        external_changes_filtered: 0,
        unsupported_changes_filtered: 0,
    };

    let current = fs::read_to_string(&path).expect("source should read");
    let proposed =
        apply_text_edits(&current, &edit.files[0].edits).expect("text edits should materialize");
    let noop = apply_text_edits(&current, &[]).expect("noop edits should materialize");
    let preview = render_unified_diff(&current, &proposed, "current/lib.rs", "proposed/lib.rs");

    assert!(preview.contains("+fn greet()"));
    assert_eq!(proposed, "fn greet() {}\n");
    assert_eq!(noop, current);
}

#[test]
fn workspace_edit_helpers_reject_invalid_ranges_and_payloads() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("lib.rs");
    fs::write(&path, "fn hello() {}\n").expect("source should write");
    let missing_text = json!({
        "changes": {
            file_uri_from_path(&path): [{
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 1 }
                }
            }]
        }
    });

    assert!(workspace_edit_from_lsp(temp.path(), &missing_text).is_err());
    let missing_document_uri = workspace_edit_from_lsp(
        temp.path(),
        &json!({
            "documentChanges": [{
                "textDocument": { "version": 1 },
                "edits": [{
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 1 }
                    },
                    "newText": "x"
                }]
            }]
        }),
    )
    .expect("missing URI document change should be filtered");
    assert_eq!(missing_document_uri.unsupported_changes_filtered, 1);
    assert!(apply_text_edits("same\n", &[]).expect("empty edits should be ok") == "same\n");
    assert_eq!(
        apply_text_edits(
            "abc",
            &[CodeTextEdit {
                range: CodeRange {
                    start_line: 1,
                    start_character: 3,
                    end_line: 1,
                    end_character: 3,
                },
                new_text: "!".to_owned(),
            }],
        )
        .expect("line-end insert should work"),
        "abc!"
    );
    assert!(
        apply_text_edits(
            "abc\n",
            &[CodeTextEdit {
                range: CodeRange {
                    start_line: 1,
                    start_character: 2,
                    end_line: 1,
                    end_character: 1,
                },
                new_text: "x".to_owned(),
            }],
        )
        .expect_err("reverse range should fail")
        .to_string()
        .contains("starts after")
    );
    assert!(
        apply_text_edits(
            "😀\n",
            &[CodeTextEdit {
                range: CodeRange {
                    start_line: 1,
                    start_character: 1,
                    end_line: 1,
                    end_character: 2,
                },
                new_text: "x".to_owned(),
            }],
        )
        .expect_err("surrogate split should fail")
        .to_string()
        .contains("surrogate")
    );
    assert!(
        apply_text_edits(
            "abc\n",
            &[CodeTextEdit {
                range: CodeRange {
                    start_line: 2,
                    start_character: 0,
                    end_line: 2,
                    end_character: 1,
                },
                new_text: "x".to_owned(),
            }],
        )
        .expect_err("bad line should fail")
        .to_string()
        .contains("outside")
    );
    assert!(
        apply_text_edits(
            "abc\n",
            &[CodeTextEdit {
                range: CodeRange {
                    start_line: 0,
                    start_character: 0,
                    end_line: 1,
                    end_character: 0,
                },
                new_text: "x".to_owned(),
            }],
        )
        .expect_err("zero line should fail")
        .to_string()
        .contains("1-based")
    );
    assert!(
        apply_text_edits(
            "abc\n",
            &[CodeTextEdit {
                range: CodeRange {
                    start_line: 1,
                    start_character: 9,
                    end_line: 1,
                    end_character: 9,
                },
                new_text: "x".to_owned(),
            }],
        )
        .expect_err("bad character should fail")
        .to_string()
        .contains("outside line")
    );
    assert_eq!(
        render_unified_diff("same\n", "same\n", "current", "proposed"),
        "No textual changes detected."
    );
}
