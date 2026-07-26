use crate::parse_gemini_model_list;

#[test]
fn gemini_catalog_filters_non_generate_content_and_keeps_page_token() {
    let page = parse_gemini_model_list(
        br#"{
            "models":[
                {"name":"models/gemini-2.5-pro","displayName":"Gemini 2.5 Pro","supportedGenerationMethods":["generateContent"]},
                {"name":"models/embedding-001","displayName":"Embedding","supportedGenerationMethods":["embedContent"]}
            ],
            "nextPageToken":"next"
        }"#,
    )
    .expect("Gemini page should parse");
    assert_eq!(page.models.len(), 1);
    assert_eq!(page.models[0].id, "models/gemini-2.5-pro");
    assert_eq!(page.next_page_token.as_deref(), Some("next"));
}
