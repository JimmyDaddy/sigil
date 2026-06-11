use serde_json::json;

use super::{REDACTED_SECRET, SecretRedactor};

#[test]
fn redacts_known_secret_values_longest_first() {
    let redactor = SecretRedactor::from_values(["sk-secret", "secret"]);

    let redacted = redactor.redact_text("token sk-secret should not show");

    assert_eq!(redacted, format!("token {REDACTED_SECRET} should not show"));
}

#[test]
fn redacts_common_assignment_and_bearer_forms() {
    let redactor = SecretRedactor::empty();

    let redacted =
        redactor.redact_text(r#"api_key="sk-one" token=sk-two Authorization: Bearer sk-three"#);

    assert!(!redacted.contains("sk-one"));
    assert!(!redacted.contains("sk-two"));
    assert!(!redacted.contains("sk-three"));
    assert!(redacted.contains(REDACTED_SECRET));
}

#[test]
fn redacts_nested_secret_like_json_keys() {
    let redactor = SecretRedactor::from_values(["plain-secret"]);
    let value = json!({
        "provider": {
            "api_key": "sk-one",
            "base_url": "https://example.com/plain-secret"
        },
        "items": [
            { "Authorization": "Bearer sk-two" },
            { "normal": "visible" }
        ]
    });

    let redacted = redactor.redact_value(&value);

    assert_eq!(redacted["provider"]["api_key"], REDACTED_SECRET);
    assert_eq!(
        redacted["provider"]["base_url"],
        format!("https://example.com/{REDACTED_SECRET}")
    );
    assert_eq!(redacted["items"][0]["Authorization"], REDACTED_SECRET);
    assert_eq!(redacted["items"][1]["normal"], "visible");
}

#[test]
fn detects_known_and_secret_like_values_for_egress() {
    let redactor = SecretRedactor::from_values(["sk-known"]);

    assert!(redactor.value_contains_secret(&json!({ "value": "sk-known" })));
    assert!(redactor.value_contains_secret(&json!({ "api_key": "sk-anything" })));
    assert!(redactor.value_contains_secret(&json!({ "headers": "Authorization: Bearer sk" })));
    assert!(!redactor.value_contains_secret(&json!({ "value": "ordinary" })));
}
