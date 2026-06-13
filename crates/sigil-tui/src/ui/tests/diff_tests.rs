use super::{
    DiffLineKind, diff_line_kind, diff_line_number_gutter, diff_line_number_text,
    diff_line_number_width, diff_line_style, number_unified_diff_lines,
};

#[test]
fn number_unified_diff_lines_tracks_old_and_new_columns() {
    let lines = [
        "--- current/note.txt",
        "+++ proposed/note.txt",
        "@@ -2,3 +2,4 @@",
        " context",
        "-old",
        "+new",
        " tail",
    ];

    let numbered = number_unified_diff_lines(lines);

    assert_eq!(numbered[0].old_line, None);
    assert_eq!(numbered[0].new_line, None);
    assert_eq!(numbered[2].old_line, None);
    assert_eq!(numbered[2].new_line, None);
    assert_eq!(numbered[3].old_line, Some(2));
    assert_eq!(numbered[3].new_line, Some(2));
    assert_eq!(numbered[4].old_line, Some(3));
    assert_eq!(numbered[4].new_line, None);
    assert_eq!(numbered[5].old_line, None);
    assert_eq!(numbered[5].new_line, Some(3));
    assert_eq!(numbered[6].old_line, Some(4));
    assert_eq!(numbered[6].new_line, Some(4));
}

#[test]
fn diff_line_number_gutter_uses_stable_columns() {
    assert_eq!(diff_line_number_gutter(Some(12), None), "12   │ ");
    assert_eq!(diff_line_number_gutter(None, Some(3)), "    3│ ");
    assert_eq!(diff_line_number_gutter(Some(4), Some(5)), " 4  5│ ");
}

#[test]
fn diff_line_number_width_uses_current_diff_max_digits() {
    let lines =
        number_unified_diff_lines(["@@ -98,4 +98,4 @@", " context", "-old", "+new", " tail"]);

    assert_eq!(diff_line_number_width(&lines), 3);
    assert_eq!(diff_line_number_text(Some(9), 3), "  9");
    assert_eq!(diff_line_number_text(None, 3), "   ");
}

#[test]
fn number_unified_diff_lines_skips_no_newline_marker_counts() {
    let lines = number_unified_diff_lines([
        "@@ -4,2 +4,2 @@",
        "-before",
        "\\ No newline at end of file",
        "+after",
    ]);

    assert_eq!(lines[1].old_line, Some(4));
    assert_eq!(lines[2].old_line, None);
    assert_eq!(lines[2].new_line, None);
    assert_eq!(lines[3].new_line, Some(4));
}

#[test]
fn number_unified_diff_lines_drops_numbering_after_invalid_hunk_header() {
    let lines = number_unified_diff_lines(["@@ invalid @@", " context", "+added"]);

    assert!(
        lines
            .iter()
            .all(|line| line.old_line.is_none() && line.new_line.is_none())
    );
}

#[test]
fn diff_line_kind_classifies_headers_hunks_changes_and_context() {
    assert_eq!(diff_line_kind("--- old"), DiffLineKind::Header);
    assert_eq!(diff_line_kind("+++ new"), DiffLineKind::Header);
    assert_eq!(diff_line_kind("diff --git a b"), DiffLineKind::Header);
    assert_eq!(diff_line_kind("index abc..def"), DiffLineKind::Header);
    assert_eq!(diff_line_kind("@@ -1 +1 @@"), DiffLineKind::Hunk);
    assert_eq!(diff_line_kind("+added"), DiffLineKind::Added);
    assert_eq!(diff_line_kind("-removed"), DiffLineKind::Removed);
    assert_eq!(diff_line_kind(" context"), DiffLineKind::Context);
}

#[test]
fn diff_line_style_covers_all_visual_variants() {
    for kind in [
        DiffLineKind::Header,
        DiffLineKind::Hunk,
        DiffLineKind::Added,
        DiffLineKind::Removed,
        DiffLineKind::Context,
    ] {
        let (_marker, style) = diff_line_style(kind);
        assert_ne!(style, ratatui::style::Style::default());
    }
}

#[test]
fn number_unified_diff_lines_handles_single_line_ranges_and_file_boundaries() {
    let lines = number_unified_diff_lines([
        "@@ -12 +34 @@",
        "-old",
        "+new",
        "diff --git a/other b/other",
        " context",
    ]);

    assert_eq!(lines[1].old_line, Some(12));
    assert_eq!(lines[2].new_line, Some(34));
    assert_eq!(lines[4].old_line, None);
    assert_eq!(lines[4].new_line, None);
}

#[test]
fn number_unified_diff_lines_handles_hunk_parse_edge_cases() {
    for header in [
        "@@not -1 +1",
        "not-a-hunk -1 +1",
        "@@",
        "@@ 1 +1 @@",
        "@@ -1 1 @@",
        "@@ -x +1 @@",
        "@@ -1 +x @@",
        "@@ - +1 @@",
    ] {
        let lines = number_unified_diff_lines([header, "+added", "-removed", " context"]);

        assert!(
            lines
                .iter()
                .all(|line| line.old_line.is_none() && line.new_line.is_none()),
            "header should disable numbering: {header}"
        );
    }
}

#[test]
fn diff_line_number_width_defaults_when_no_lines_are_numbered() {
    let lines = number_unified_diff_lines(["diff --git a/a b/a", "index abc..def", "+outside"]);

    assert_eq!(diff_line_number_width(&lines), 2);
    assert_eq!(diff_line_number_text(Some(123), 5), "  123");
}
