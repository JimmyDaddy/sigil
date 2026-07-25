use crate::{OpenAiModelAdmission, parse_openai_responses_model_list};

#[test]
fn openai_catalog_filters_known_non_generation_and_keeps_unknown_unverified() {
    let models = parse_openai_responses_model_list(
        br#"{"object":"list","data":[
            {"id":"gpt-5","object":"model","owned_by":"openai"},
            {"id":"text-embedding-3-small","object":"model","owned_by":"openai"},
            {"id":"account/deployment","object":"model","owned_by":"account"}
        ]}"#,
    )
    .expect("OpenAI list should parse");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].admission, OpenAiModelAdmission::KnownGeneration);
    assert_eq!(
        models[1].admission,
        OpenAiModelAdmission::UnverifiedGeneration
    );
}
