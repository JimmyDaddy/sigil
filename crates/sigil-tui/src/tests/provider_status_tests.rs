use serde_json::json;

use super::parse_remote_model_ids;

#[test]
fn parse_remote_model_ids_keeps_order_and_deduplicates() {
    let payload = json!({
        "data": [
            {"id": "deepseek-v4-flash"},
            {"id": "deepseek-v4-pro"},
            {"id": "deepseek-v4-flash"},
            {"missing": true}
        ]
    });

    assert_eq!(
        parse_remote_model_ids(&payload),
        vec!["deepseek-v4-flash", "deepseek-v4-pro"]
    );
}
