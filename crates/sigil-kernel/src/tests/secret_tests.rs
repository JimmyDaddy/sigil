use serde_json::json;

use super::{REDACTED_SECRET, SecretRedactor};
use crate::SecretString;

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

#[test]
fn secret_redactor_ignores_short_duplicates_and_empty_inputs() {
    let mut redactor = SecretRedactor::empty();
    redactor.add_secret("abc");
    redactor.add_secret("sk-live-123");
    redactor.add_secret("sk-live-123");

    assert_eq!(redactor.redact_text(""), "");
    assert_eq!(
        redactor.redact_text("token sk-live-123"),
        format!("token {REDACTED_SECRET}")
    );
}

#[test]
fn secret_carrier_redacts_short_values_keys_and_debug_output() {
    let mut redactor = SecretRedactor::empty();
    redactor.add_secret_carrier(SecretString::new("abc"));

    assert_eq!(
        redactor.redact_value(&json!({"abc": "value", "nested": "abc"})),
        json!({(REDACTED_SECRET): "value", "nested": REDACTED_SECRET})
    );
    assert!(redactor.value_contains_secret(&json!({"abc": "value"})));
    assert!(!format!("{redactor:?}").contains("abc"));

    let mut numeric = SecretRedactor::empty();
    numeric.add_secret_carrier(SecretString::new("1"));
    assert_eq!(numeric.redact_value(&json!(1)), json!(REDACTED_SECRET));
    assert!(numeric.value_contains_secret(&json!(1)));
}

#[test]
fn short_carriers_never_rescan_or_survive_the_selected_replacement() {
    let carriers = ["[", "r", "e", "d", "a", "c", "t", "]"];
    let mut redactor = SecretRedactor::empty();
    for carrier in carriers {
        redactor.add_secret_carrier(SecretString::new(carrier));
    }

    let input = "[redacted]".repeat((4 * 1024 * 1024) / "[redacted]".len());
    let output = redactor.redact_text(&input);

    assert!(output.len() <= input.len() + REDACTED_SECRET.len());
    for carrier in carriers {
        assert!(!output.contains(carrier));
    }
}

#[test]
fn unavailable_safe_marker_cannot_create_structural_credentials() {
    let mut redactor = SecretRedactor::empty();
    for carrier in ["[", "<", "*"] {
        redactor.add_secret_carrier(SecretString::new(carrier));
    }

    assert_eq!(redactor.redact_text("Bearer[ raw-token"), "");
    assert_eq!(redactor.redact_text("token[=raw-token"), "");
}

#[test]
fn high_density_structural_credentials_have_bounded_output() {
    let input = "Bearer x token=y ".repeat(200_000);

    let output = SecretRedactor::empty().redact_text(&input);

    assert!(output.len() <= input.len() + REDACTED_SECRET.len());
    assert!(!output.contains("Bearer x"));
    assert!(!output.contains("token=y"));
}

#[test]
fn carrier_budget_overflow_fails_closed_without_leaking_unknown_values() {
    let mut count_limited = SecretRedactor::empty();
    for index in 0..=super::MAX_SECRET_CARRIERS {
        count_limited.add_secret_carrier(SecretString::new(format!("carrier-{index}")));
    }
    assert_eq!(count_limited.redact_text("ordinary-visible-text"), "");
    assert_eq!(
        count_limited.redact_value(&json!({"normal": 42})),
        json!({"": ""})
    );

    let mut byte_limited = SecretRedactor::empty();
    byte_limited.add_secret_carrier(SecretString::new(
        "x".repeat(super::MAX_SECRET_CARRIER_BYTES + 1),
    ));
    assert_eq!(byte_limited.redact_text("anything"), "");
}

#[test]
fn truncated_byte_redaction_removes_every_non_ascii_secret_prefix() {
    let secret = "密钥🙂终";
    let prefix = b"diagnostic ";
    let redactor = SecretRedactor::from_values([secret]);
    let mut body = prefix.to_vec();
    body.extend_from_slice(secret.as_bytes());
    body.extend_from_slice(b" trailing");

    for secret_prefix_len in 1..=secret.len() {
        let captured = &body[..prefix.len() + secret_prefix_len];
        assert_eq!(
            redactor.redact_truncated_bytes(captured),
            "diagnostic [redacted]",
            "secret prefix length {secret_prefix_len} must be removed before UTF-8 decoding"
        );
    }
}

#[test]
fn truncated_head_tail_redaction_removes_split_and_longest_overlapping_secrets() {
    let redactor = SecretRedactor::from_values(["token-alpha-long", "token-beta"]);
    let (head, tail) =
        redactor.redact_truncated_head_tail_bytes(b"before token-alpha-lo", b"ng after");
    assert_eq!(head, "before [redacted]");
    assert_eq!(tail, "[redacted] after");

    let overlapping = SecretRedactor::from_values(["abcd", "axyz"]);
    assert_eq!(
        overlapping.redact_truncated_bytes(b"prefix ax"),
        "prefix [redacted]"
    );
}

#[test]
fn truncated_boundary_overlap_is_linear_for_long_repeated_secrets() {
    let prefix_secret = format!("{}b", "a".repeat(30_000));
    let suffix_secret = format!("b{}", "a".repeat(30_000));
    let redactor = SecretRedactor::from_values([prefix_secret, suffix_secret]);
    let repeated = "a".repeat(30_000);

    assert_eq!(
        redactor.redact_truncated_bytes(format!("prefix {repeated}").as_bytes()),
        "prefix [redacted]"
    );
    let (head, tail) = redactor
        .redact_truncated_head_tail_bytes(b"ordinary head", format!("{repeated} tail").as_bytes());
    assert_eq!(head, "ordinary head");
    assert_eq!(tail, "[redacted] tail");
}

#[test]
fn secret_helpers_cover_boundaries_empty_values_and_passthrough_cases() {
    assert!(super::secret_like_key("API-Key"));
    assert!(!super::secret_like_key("username"));

    assert!(!super::value_has_non_empty_data(&serde_json::Value::Null));
    assert!(super::value_has_non_empty_data(&json!(false)));
    assert!(!super::value_has_non_empty_data(&json!("   ")));
    assert!(super::value_has_non_empty_data(&json!(["token"])));
    assert!(super::value_has_non_empty_data(&json!({"nested": []})));

    assert_eq!(
        super::redact_bearer_tokens("foobearer abc"),
        "foobearer abc"
    );
    assert_eq!(super::redact_bearer_tokens("Bearer "), "Bearer ");

    assert_eq!(
        super::redact_secret_assignments("mytoken=value token:"),
        "mytoken=value token:"
    );
    assert_eq!(
        super::redact_secret_assignments("token='value' api_key = plain"),
        format!("token='{REDACTED_SECRET}' api_key = {REDACTED_SECRET}")
    );
    assert_eq!(
        super::redact_secret_assignments(r#"token="prefix\"RAW_SECRET" trailing"#),
        format!(r#"token="{REDACTED_SECRET}" trailing"#)
    );
    assert_eq!(
        super::redact_secret_assignments(r"token='prefix\'RAW_SECRET' trailing"),
        format!("token='{REDACTED_SECRET}' trailing")
    );
    assert_eq!(
        super::redact_secret_assignments(r#"token="prefix\\" trailing"#),
        format!(r#"token="{REDACTED_SECRET}" trailing"#)
    );

    let redactor = SecretRedactor::empty();
    assert_eq!(redactor.redact_value(&json!(42)), json!(42));
}
