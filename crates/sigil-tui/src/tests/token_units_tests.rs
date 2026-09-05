use super::*;

#[test]
fn parses_decimal_units_case_insensitively() {
    assert_eq!(parse_optional_token_count("256K"), Ok(Some(256_000)));
    assert_eq!(parse_optional_token_count("1m"), Ok(Some(1_000_000)));
    assert_eq!(parse_optional_token_count("64000"), Ok(Some(64_000)));
    assert_eq!(parse_optional_token_count("  "), Ok(None));
}

#[test]
fn rejects_zero_invalid_and_overflow_values() {
    assert_eq!(parse_optional_token_count("0"), Err(TokenCountError::Zero));
    assert!(matches!(
        parse_optional_token_count("1.5M"),
        Err(TokenCountError::Invalid(_))
    ));
    assert!(matches!(
        parse_optional_token_count("5000M"),
        Err(TokenCountError::OutOfRange(_))
    ));
}

#[test]
fn cycles_presets_and_resets_unknown_custom_values() {
    assert_eq!(cycle_context_window_preset("", false), "64K");
    assert_eq!(cycle_context_window_preset("256000", false), "1M");
    assert_eq!(cycle_context_window_preset("1M", true), "256K");
    assert_eq!(cycle_context_window_preset("12345", false), "");
}
