use super::*;

#[test]
fn wrapped_line_rows_counts_visual_rows() {
    assert_eq!(wrapped_line_rows("", 10), 1);
    assert_eq!(wrapped_line_rows("short", 10), 1);
    assert_eq!(wrapped_line_rows("1234567890", 10), 1);
    assert_eq!(wrapped_line_rows("12345678901", 10), 2);
    assert_eq!(wrapped_line_rows("你好", 2), 2);
}
