use anyhow::Result;

use crate::{
    CacheLayoutMutationKind, CacheLayoutProofV1, CompletionRequest, HostedToolKind,
    HostedToolLimits, HostedToolRequest, MessageRole, ModelMessage, ReasoningEffort, ToolAccess,
    ToolCategory, ToolPreviewCapability, ToolSpec, canonicalize_cache_stable_json,
};

fn request() -> CompletionRequest {
    let mut system = ModelMessage::user("stable system");
    system.role = MessageRole::System;
    CompletionRequest {
        provider_name: "provider-a".to_owned(),
        model_name: "model-a".to_owned(),
        messages: vec![system, ModelMessage::user("first user turn")],
        tools: vec![ToolSpec {
            name: "read".to_owned(),
            description: "read a file".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }],
        temperature: Some(0.2),
        max_tokens: Some(1024),
        reasoning_effort: Some(ReasoningEffort::High),
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: Some("partition-a".to_owned()),
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    }
}

#[test]
fn cache_stable_json_sorts_nested_tool_schema_keys_without_reordering_arrays() -> Result<()> {
    let mut root = serde_json::Map::new();
    root.insert("z".to_owned(), serde_json::json!({"b": 2, "a": 1}));
    root.insert("a".to_owned(), serde_json::json!(["second", "first"]));

    let canonical = canonicalize_cache_stable_json(&serde_json::Value::Object(root))?;

    assert_eq!(
        serde_json::to_string(&canonical)?,
        r#"{"a":["second","first"],"z":{"a":1,"b":2}}"#
    );
    Ok(())
}

#[test]
fn cache_layout_proof_is_deterministic_hash_only_and_validates() -> Result<()> {
    let request = request();
    let proof = CacheLayoutProofV1::from_request(&request, None)?;
    let same = CacheLayoutProofV1::from_request(&request, None)?;

    assert_eq!(proof, same);
    proof.validate()?;
    assert_eq!(
        proof.mutation_from_previous.kind,
        CacheLayoutMutationKind::FirstObservation
    );
    let rendered = serde_json::to_string(&proof)?;
    assert!(!rendered.contains("stable system"));
    assert!(!rendered.contains("first user turn"));
    assert!(!rendered.contains("partition-a"));
    assert!(!rendered.contains("read a file"));
    Ok(())
}

#[test]
fn cache_layout_proof_classifies_identical_append_and_old_history_rewrite() -> Result<()> {
    let baseline_request = request();
    let baseline = CacheLayoutProofV1::from_request(&baseline_request, None)?;
    let identical = CacheLayoutProofV1::from_request(&baseline_request, Some(&baseline))?;
    assert_eq!(
        identical.mutation_from_previous.kind,
        CacheLayoutMutationKind::Identical
    );

    let mut appended_request = baseline_request.clone();
    appended_request.messages.push(ModelMessage::assistant(
        Some("answer".to_owned()),
        Vec::new(),
    ));
    let appended = CacheLayoutProofV1::from_request(&appended_request, Some(&baseline))?;
    assert_eq!(
        appended.mutation_from_previous.kind,
        CacheLayoutMutationKind::ConversationTailAppended
    );
    assert_eq!(
        appended
            .mutation_from_previous
            .reusable_conversation_message_count,
        baseline.conversation_message_count
    );

    let mut rewritten_request = appended_request;
    rewritten_request.messages[1] = ModelMessage::user("rewritten first turn");
    let rewritten = CacheLayoutProofV1::from_request(&rewritten_request, Some(&baseline))?;
    assert_eq!(
        rewritten.mutation_from_previous.kind,
        CacheLayoutMutationKind::ConversationHistoryRewritten
    );
    assert!(
        !rewritten
            .mutation_from_previous
            .local_stable_prefix_preserved
    );
    Ok(())
}

#[test]
fn cache_layout_proof_classifies_route_system_tool_and_dynamic_changes() -> Result<()> {
    let baseline_request = request();
    let baseline = CacheLayoutProofV1::from_request(&baseline_request, None)?;

    let mut route = baseline_request.clone();
    route.model_name = "model-b".to_owned();
    assert_eq!(
        CacheLayoutProofV1::from_request(&route, Some(&baseline))?
            .mutation_from_previous
            .kind,
        CacheLayoutMutationKind::RouteChanged
    );

    let mut system = baseline_request.clone();
    system.messages[0].content = Some("changed system".to_owned());
    assert_eq!(
        CacheLayoutProofV1::from_request(&system, Some(&baseline))?
            .mutation_from_previous
            .kind,
        CacheLayoutMutationKind::SystemChanged
    );

    let mut tools = baseline_request.clone();
    tools.tools[0].description = "changed tool".to_owned();
    assert_eq!(
        CacheLayoutProofV1::from_request(&tools, Some(&baseline))?
            .mutation_from_previous
            .kind,
        CacheLayoutMutationKind::ToolSchemaChanged
    );

    let mut dynamic = baseline_request;
    dynamic.temperature = Some(0.7);
    let dynamic = CacheLayoutProofV1::from_request(&dynamic, Some(&baseline))?;
    assert_eq!(
        dynamic.mutation_from_previous.kind,
        CacheLayoutMutationKind::DynamicStateOnly
    );
    assert!(dynamic.mutation_from_previous.local_stable_prefix_preserved);
    Ok(())
}

#[test]
fn hosted_tool_removal_is_a_tool_schema_break_not_dynamic_state() -> Result<()> {
    let mut baseline_request = request();
    baseline_request.hosted_tools.push(HostedToolRequest::new(
        "authorization-1",
        HostedToolKind::WebSearch,
        HostedToolLimits::default(),
    )?);
    let baseline = CacheLayoutProofV1::from_request(&baseline_request, None)?;

    let mut summary_request = baseline_request;
    summary_request.hosted_tools.clear();
    summary_request
        .messages
        .push(ModelMessage::user("semantic summary instruction"));
    let summary = CacheLayoutProofV1::from_request(&summary_request, Some(&baseline))?;

    assert_eq!(
        summary.mutation_from_previous.kind,
        CacheLayoutMutationKind::ToolSchemaChanged
    );
    assert!(!summary.mutation_from_previous.local_stable_prefix_preserved);
    Ok(())
}
