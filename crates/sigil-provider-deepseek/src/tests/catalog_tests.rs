use crate::{BUNDLED_DEEPSEEK_MODELS, parse_deepseek_model_list};

#[test]
fn bundled_catalog_includes_the_exact_vision_experiment_model_id() {
    assert!(BUNDLED_DEEPSEEK_MODELS.iter().any(|(id, label, default)| {
        *id == "deepseek-v4-flash-vision-exp"
            && *label == "DeepSeek V4 Flash Vision (Experimental)"
            && !default
    }));
}

#[test]
fn deepseek_catalog_parses_official_shape_and_rejects_wrong_owner() {
    let models = parse_deepseek_model_list(
        br#"{"object":"list","data":[{"id":"deepseek-v4-flash","object":"model","owned_by":"deepseek"}]}"#,
    )
    .expect("official list should parse");
    assert_eq!(models[0].id, "deepseek-v4-flash");
    assert!(
        parse_deepseek_model_list(
            br#"{"object":"list","data":[{"id":"foreign","object":"model","owned_by":"other"}]}"#
        )
        .is_err()
    );
}
