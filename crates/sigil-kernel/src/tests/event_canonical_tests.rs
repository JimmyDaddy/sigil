use super::{canonical_json_bytes, canonical_json_content_hash};

#[test]
fn canonical_json_existing_numeric_and_string_goldens_remain_byte_stable() {
    // These are compatibility vectors for the existing algorithm, not a JCS certification.
    // In particular, changing the saturating i64/f64 boundary requires a separate scheme review.
    let vectors = [
        (
            "[-9223372036854775808,9223372036854775807,18446744073709551615]",
            "[-9223372036854775808,9223372036854775807,18446744073709551615]",
            "f7a5aa78925e65cb1ae66f5685b5c375450013254e1e8eaac9fd493775fab725",
        ),
        (
            "1.0",
            "1",
            "6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b",
        ),
        (
            "1e0",
            "1",
            "6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b",
        ),
        (
            "-0.0",
            "0",
            "5feceb66ffc86f38d952786c6d696c79c2dbc239dd4e91b46729d73a27fb57e9",
        ),
        (
            "1.5",
            "1.5",
            "9f29a130438b81170b92a42650f9a94291ecad60bd47af2a3886e75f7f728725",
        ),
        (
            "1e20",
            "1e+20",
            "7c18c9fbdcc8281573e9db9e04f04c3790b10696f3706f0f03fa87427d33e28b",
        ),
        (
            "9223372036854775808.0",
            "9223372036854775807",
            "b34a1c30a715f6bf8b7243afa7fab883ce3612b7231716bdcbbdc1982e1aed29",
        ),
        (
            r#"{"z":"λ","a":{"array":[true,null,"引号\"与换行\n"]}}"#,
            r#"{"a":{"array":[true,null,"引号\"与换行\n"]},"z":"λ"}"#,
            "9f2b5b9befb95f783d8a42dd198ff4fe6cc882c94d7e145c7f6f6dcc94f09a59",
        ),
    ];
    for (input, bytes, hash) in vectors {
        let value = serde_json::from_str(input).expect("valid golden JSON");
        assert_eq!(
            canonical_json_bytes(&value).expect("canonical bytes"),
            bytes.as_bytes(),
            "input: {input}"
        );
        assert_eq!(
            canonical_json_content_hash(&value).expect("canonical hash"),
            format!("sha256:jcs-v1:{hash}"),
            "input: {input}"
        );
    }
}

#[test]
fn stored_event_current_numeric_fixture_verifies_and_roundtrips() {
    // A current-schema envelope golden, including the pre-existing saturating number boundary.
    // This checks the checksum reader/writer, not typed ToolExecutionStarted payload decoding.
    let wire = r#"{"schema_version":2,"event_type":"tool_execution_started","event_version":1,"event_class":"critical","event_id":"canonical-event","session_id":"canonical-session","stream_sequence":1,"record_checksum":"sha256:jcs-v1:1869a4deec5868e25446543ee8481d987c3e4908e8416c73bfe4ec0ad87e56b1","payload":{"numbers":[1.0,-0.0,1.5,1e20,9223372036854775808.0]}}"#;
    let event = super::StoredEvent::from_json_str(wire).expect("current fixture checksum");
    let serialized = event.to_json_line().expect("current envelope writer");
    let reopened = super::StoredEvent::from_json_str(&serialized).expect("current envelope reader");
    assert_eq!(event, reopened);
    assert_eq!(
        reopened.record_checksum,
        "sha256:jcs-v1:1869a4deec5868e25446543ee8481d987c3e4908e8416c73bfe4ec0ad87e56b1"
    );
}
