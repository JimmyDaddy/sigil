use super::super::formatting::{
    build_model_picker_options, char_to_byte_index, non_empty_or,
    normalize_command_prefix_character, normalize_runtime_model, sidebar_width_for_terminal,
    summarize_error,
};

#[test]
fn summarize_error_prefers_last_non_empty_cause_line() {
    let summary = summarize_error(
        "request failed\n\nCaused by:\n  0: upstream timeout\n  1: socket closed unexpectedly",
    );

    assert_eq!(summary, "socket closed unexpectedly");
}

#[test]
fn normalize_runtime_model_maps_aliases_and_trims() {
    assert_eq!(
        normalize_runtime_model("  flash "),
        Some("deepseek-v4-flash".to_owned())
    );
    assert_eq!(
        normalize_runtime_model("v4-pro"),
        Some("deepseek-v4-pro".to_owned())
    );
    assert_eq!(normalize_runtime_model("   "), None);
}

#[test]
fn build_model_picker_options_uses_known_models_and_appends_custom_current() {
    let options = build_model_picker_options(" custom-model ", Vec::new());

    assert!(options.iter().any(|option| option == "deepseek-v4-flash"));
    assert!(options.iter().any(|option| option == "custom-model"));
}

#[test]
fn utility_formatters_cover_threshold_and_unicode_edges() {
    assert_eq!(sidebar_width_for_terminal(95), 0);
    assert_eq!(sidebar_width_for_terminal(96), 24);
    assert_eq!(sidebar_width_for_terminal(160), 42);
    assert_eq!(normalize_command_prefix_character('、'), Some('/'));
    assert_eq!(normalize_command_prefix_character('x'), None);
    assert_eq!(char_to_byte_index("a中b", 2), "a中".len());
    assert_eq!(non_empty_or("   ", "fallback"), "fallback");
}
