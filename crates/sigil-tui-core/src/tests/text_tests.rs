use super::BidiText;

#[test]
fn bidi_mapping_is_a_bijection_for_mixed_direction_text() {
    let text = BidiText::new("left אבג right").expect("bidi text");
    assert_ne!(text.logical(), text.visual());
    assert_eq!(
        text.map().visual_to_logical().len(),
        text.logical().chars().count()
    );
    for (visual, logical) in text.map().visual_to_logical().iter().copied().enumerate() {
        assert_eq!(text.map().visual_index_for_logical(logical), Some(visual));
    }
}

#[test]
fn bidi_mapping_preserves_format_controls_in_emoji_sequences() {
    let text = BidiText::new("中文 אבג 👩‍💻").expect("bidi text");
    assert_eq!(
        text.map().visual_to_logical().len(),
        text.logical().chars().count()
    );
    for (visual, logical) in text.map().visual_to_logical().iter().copied().enumerate() {
        assert_eq!(text.map().visual_index_for_logical(logical), Some(visual));
    }
    assert!(text.visual().contains('‍'));
}
