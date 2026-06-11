use super::{
    diff_line_number_gutter, diff_line_number_text, diff_line_number_width,
    number_unified_diff_lines,
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
