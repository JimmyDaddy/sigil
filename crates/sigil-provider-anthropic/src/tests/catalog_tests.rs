use crate::parse_anthropic_model_list;

#[test]
fn anthropic_catalog_maps_display_names_and_after_id_pagination() {
    let page = parse_anthropic_model_list(
        br#"{
            "data":[{"id":"claude-sonnet-4-5","display_name":"Claude Sonnet 4.5","created_at":"2025-01-01","type":"model"}],
            "has_more":true,
            "first_id":"claude-sonnet-4-5",
            "last_id":"claude-sonnet-4-5"
        }"#,
    )
    .expect("Anthropic page should parse");
    assert_eq!(page.models[0].display_name, "Claude Sonnet 4.5");
    assert_eq!(page.next_after_id.as_deref(), Some("claude-sonnet-4-5"));
}
