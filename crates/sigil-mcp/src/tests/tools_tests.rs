use super::*;

#[test]
fn mcp_approval_preview_keeps_request_arguments_without_claiming_file_changes() -> Result<()> {
    let args = json!({"text":"中文 👩‍💻", "count":2, "nested":{"enabled":true}});
    let preview = mcp_tool_approval_preview("fixture", "echo", &args, &SecretRedactor::empty())?
        .expect("bounded request should have a review");
    let body: Value = serde_json::from_str(&preview.body)?;
    assert_eq!(
        body,
        json!({"server":"fixture", "tool":"echo", "arguments":args})
    );
    assert!(preview.changed_files.is_empty());
    assert!(preview.file_diffs.is_empty());
    let changed = mcp_tool_approval_preview(
        "fixture",
        "echo",
        &json!({"text":"different exact request"}),
        &SecretRedactor::empty(),
    )?
    .expect("second bounded request should have a review");
    assert_ne!(preview.body, changed.body);
    Ok(())
}

#[test]
fn mcp_approval_preview_redacts_credentials_and_private_paths() -> Result<()> {
    let known_secret = "fixture-known-credential";
    let args = json!({
        "text": format!("prefix {known_secret} suffix"),
        "api_key": "fixture-field-credential",
        "path": "/Users/fixture/private/repository/data.txt",
    });
    let redactor = SecretRedactor::from_values([known_secret]);
    let preview = mcp_tool_approval_preview("fixture", "echo", &args, &redactor)?
        .expect("redacted bounded review should remain available");
    assert!(!preview.body.contains(known_secret));
    assert!(!preview.body.contains("fixture-field-credential"));
    assert!(!preview.body.contains("/Users/fixture"));
    assert_eq!(args["api_key"], "fixture-field-credential");
    Ok(())
}

#[test]
fn mcp_approval_preview_redacts_nested_local_path_values_without_removing_other_arguments()
-> Result<()> {
    let paths = [
        "/Users/fixture/private.txt",
        "C:\\Users\\fixture\\private.txt",
        "C:/Users/fixture/private.txt",
        "\\\\server\\private\\file",
        "~/private/file",
        "../outside/file",
        "file:///Users/fixture/private.txt",
        "Read '/Users/fixture/private.txt' before continuing.",
        "路径：/Users/fixture/private.txt",
        "path:/Users/fixture/private.txt",
        "</Users/fixture/private.txt>",
        "Read “/Users/fixture/private.txt” now",
        "请读「/Users/fixture/private.txt」",
        "请读/Users/fixture/private.txt",
    ];
    for path in paths {
        let preview = mcp_tool_approval_preview(
            "fixture",
            "echo",
            &json!({
                "nested": [{"value":path}], "relative":"src/main.rs", "text":"中文 retained",
            }),
            &SecretRedactor::empty(),
        )?
        .expect("redacted bounded request should remain reviewable");
        let body: Value = serde_json::from_str(&preview.body)?;
        assert_eq!(
            body["arguments"]["nested"][0]["value"],
            "[local path redacted]"
        );
        assert_eq!(body["arguments"]["relative"], "src/main.rs");
        assert_eq!(body["arguments"]["text"], "中文 retained");
        assert!(!preview.body.contains("fixture/private"));
    }
    assert!(
        mcp_tool_approval_preview(
            "fixture",
            "echo",
            &json!({
                "/Users/fixture/one":"a", "/Users/fixture/two":"b",
            }),
            &SecretRedactor::empty()
        )?
        .is_none(),
        "private keys must not be merged by redaction"
    );
    for url in ["https://example.com/public", "http://example.com/path/one"] {
        let preview = mcp_tool_approval_preview(
            "fixture",
            "echo",
            &json!({"url":url}),
            &SecretRedactor::empty(),
        )?
        .expect("public URL remains reviewable");
        assert_eq!(
            serde_json::from_str::<Value>(&preview.body)?["arguments"]["url"],
            url
        );
    }
    Ok(())
}

#[test]
fn mcp_approval_preview_rejects_private_keys_before_redaction_can_merge_them() -> Result<()> {
    let redactor = SecretRedactor::from_values(["private-A", "private-B"]);
    for args in [
        json!({"private-A":{"action":"one"}, "private-B":{"action":"two"}}),
        json!({"nested":{"private-A":{"action":"one"}, "private-B":{"action":"two"}}}),
        json!({"nested":[{"private-A":{"action":"one"}}, {"private-B":{"action":"two"}}]}),
    ] {
        let original = args.clone();
        assert!(mcp_tool_approval_preview("fixture", "echo", &args, &redactor)?.is_none());
        assert_eq!(
            args, original,
            "review projection must not alter the exact call"
        );
    }
    // A single rewritten key is also unavailable; completeness cannot depend on a collision.
    assert!(
        mcp_tool_approval_preview(
            "fixture",
            "echo",
            &json!({
                "private-A":{"action":"one"}, "ordinary":{"action":"two"},
            }),
            &redactor
        )?
        .is_none()
    );
    assert!(
        mcp_tool_approval_preview(
            "fixture",
            "echo",
            &json!({
                "Bearer fixture-bearer-credential":{"action":"one"},
            }),
            &SecretRedactor::empty()
        )?
        .is_none()
    );
    Ok(())
}

#[test]
fn mcp_approval_preview_never_truncates_an_oversized_action_into_approvable_text() -> Result<()> {
    let redactor = SecretRedactor::empty();
    for (server, tool, args) in [
        (
            "fixture".to_owned(),
            "echo".to_owned(),
            json!({"text":"x".repeat(MCP_APPROVAL_REVIEW_MAX_BYTES)}),
        ),
        ("s".repeat(513), "echo".to_owned(), json!({})),
        ("fixture".to_owned(), "t".repeat(513), json!({})),
        // Escaping in the complete pretty-printed review also counts against the bound.
        (
            "fixture".to_owned(),
            "echo".to_owned(),
            json!({"text":"\n".repeat(MCP_APPROVAL_REVIEW_MAX_BYTES / 2 - 20)}),
        ),
    ] {
        assert!(mcp_tool_approval_preview(&server, &tool, &args, &redactor)?.is_none());
    }
    Ok(())
}
