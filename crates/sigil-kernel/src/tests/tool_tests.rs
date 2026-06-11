use crate::{ToolDiffBudget, ToolDiffStats, ToolPreview, ToolPreviewFile, ToolPreviewSnapshot};

#[test]
fn tool_diff_stats_ignore_file_headers() {
    let stats = ToolDiffStats::from_unified_diff(
        "--- a/file.txt\n+++ b/file.txt\n@@ -1,2 +1,3 @@\n old\n-removed\n+added\n+another",
    );

    assert_eq!(stats.added, 2);
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.hunks, 1);
}

#[test]
fn preview_snapshot_builder_truncates_by_file_and_line_budget() {
    let preview = ToolPreview {
        title: "Write file".to_owned(),
        summary: "Update two files".to_owned(),
        body: "preview body".to_owned(),
        changed_files: vec!["a.txt".to_owned(), "b.txt".to_owned()],
        file_diffs: vec![
            ToolPreviewFile {
                path: "a.txt".to_owned(),
                diff: "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n-old\n+new\n+extra".to_owned(),
            },
            ToolPreviewFile {
                path: "b.txt".to_owned(),
                diff: "--- a/b.txt\n+++ b/b.txt\n@@ -0,0 +1 @@\n+created".to_owned(),
            },
        ],
    };

    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-1",
        "write_file",
        &preview,
        ToolDiffBudget {
            max_files: 1,
            max_lines_total: 5,
            max_lines_per_file: 5,
            max_bytes_total: 1024,
            max_bytes_per_file: 1024,
        },
        Some("preview-hash".to_owned()),
    );

    assert_eq!(snapshot.call_id, "call-1");
    assert_eq!(snapshot.tool_name, "write_file");
    assert_eq!(
        snapshot.original_preview_hash.as_deref(),
        Some("preview-hash")
    );
    assert!(snapshot.truncated);
    assert_eq!(snapshot.file_diffs.len(), 1);
    assert_eq!(snapshot.file_diffs[0].path, "a.txt");
    assert_eq!(snapshot.file_diffs[0].rendered_line_count, 5);
    assert!(snapshot.file_diffs[0].truncated);
    assert_eq!(snapshot.original_stats.added, 3);
    assert_eq!(snapshot.original_stats.removed, 1);
    assert_eq!(snapshot.original_stats.hunks, 2);
    assert_eq!(snapshot.rendered_stats.added, 1);
    assert_eq!(snapshot.rendered_stats.removed, 1);
    assert_eq!(snapshot.rendered_stats.hunks, 1);
}

#[test]
fn preview_snapshot_builder_truncates_by_byte_budget() {
    let preview = ToolPreview {
        title: "Write file".to_owned(),
        summary: "Update file".to_owned(),
        body: "preview body".to_owned(),
        changed_files: vec!["a.txt".to_owned()],
        file_diffs: vec![ToolPreviewFile {
            path: "a.txt".to_owned(),
            diff: "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new".to_owned(),
        }],
    };

    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-1",
        "write_file",
        &preview,
        ToolDiffBudget {
            max_files: 1,
            max_lines_total: 160,
            max_lines_per_file: 160,
            max_bytes_total: 20,
            max_bytes_per_file: 20,
        },
        None,
    );

    assert!(snapshot.truncated);
    assert!(snapshot.file_diffs[0].truncated);
    assert!(snapshot.rendered_byte_count <= 20);
    assert!(snapshot.rendered_line_count < snapshot.original_line_count);
}
