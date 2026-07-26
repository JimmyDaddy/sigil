use crate::parse_openai_compatible_model_list;

#[test]
fn compatible_catalog_keeps_custom_deployment_ids_and_rejects_missing_ids() {
    let models = parse_openai_compatible_model_list(
        br#"{"data":[{"id":"deployment/blue"},{"id":"vendor:model"}]}"#,
    )
    .expect("compatible list should parse");
    assert_eq!(models.len(), 2);
    assert!(parse_openai_compatible_model_list(br#"{"data":[{}]}"#).is_err());
}
