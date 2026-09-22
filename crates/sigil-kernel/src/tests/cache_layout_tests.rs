use anyhow::Result;

use super::CacheLayoutProofV2;

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

#[test]
fn cache_layout_v2_ignores_per_turn_hosted_authorization_identity() -> Result<()> {
    let limits = HostedToolLimits {
        max_uses: Some(3),
        allowed_domains: vec!["docs.example.com/reference".to_owned()],
        blocked_domains: Vec::new(),
    };
    let mut first_request = request();
    first_request.hosted_tools.push(HostedToolRequest::new(
        "authorization-turn-1",
        HostedToolKind::WebSearch,
        limits.clone(),
    )?);
    let first = CacheLayoutProofV2::from_request(&first_request, None)?;

    let mut next_request = first_request.clone();
    next_request.hosted_tools = vec![HostedToolRequest::new(
        "authorization-turn-2",
        HostedToolKind::WebSearch,
        limits,
    )?];
    let next = CacheLayoutProofV2::from_request(&next_request, Some(&first))?;

    first.validate()?;
    next.validate()?;
    assert_eq!(first.tool_schema_hash, next.tool_schema_hash);
    assert_eq!(first.layout_hash, next.layout_hash);
    assert_eq!(
        next.mutation_from_previous.kind,
        CacheLayoutMutationKind::Identical
    );

    let legacy_first = CacheLayoutProofV1::from_request(&first_request, None)?;
    let legacy_next = CacheLayoutProofV1::from_request(&next_request, Some(&legacy_first))?;
    assert_eq!(
        legacy_next.mutation_from_previous.kind,
        CacheLayoutMutationKind::ToolSchemaChanged
    );
    Ok(())
}

#[test]
fn cache_layout_v2_ignores_host_only_message_identity_and_presentation_fields() -> Result<()> {
    let first_request = request();
    let first = CacheLayoutProofV2::from_request(&first_request, None)?;

    let mut next_request = first_request;
    next_request.messages[1].id = "new-local-message-id".to_owned();
    next_request.messages[1].assistant_kind = Some(crate::AssistantMessageKind::Progress);
    next_request.messages[1].logical_run_id = Some(crate::LogicalRunId::new("run-2")?);
    let next = CacheLayoutProofV2::from_request(&next_request, Some(&first))?;

    assert_eq!(first.conversation_hash, next.conversation_hash);
    assert_eq!(
        next.mutation_from_previous.kind,
        CacheLayoutMutationKind::Identical
    );
    assert!(next.mutation_from_previous.local_stable_prefix_preserved);
    Ok(())
}

#[test]
fn cache_layout_v2_detects_provider_visible_hosted_schema_changes() -> Result<()> {
    let mut baseline_request = request();
    baseline_request.hosted_tools.push(HostedToolRequest::new(
        "authorization-1",
        HostedToolKind::WebSearch,
        HostedToolLimits {
            max_uses: Some(2),
            ..HostedToolLimits::default()
        },
    )?);
    let baseline = CacheLayoutProofV2::from_request(&baseline_request, None)?;

    let mut changed_request = baseline_request;
    changed_request.hosted_tools = vec![HostedToolRequest::new(
        "authorization-2",
        HostedToolKind::WebSearch,
        HostedToolLimits {
            max_uses: Some(4),
            ..HostedToolLimits::default()
        },
    )?];
    let changed = CacheLayoutProofV2::from_request(&changed_request, Some(&baseline))?;

    assert_ne!(baseline.tool_schema_hash, changed.tool_schema_hash);
    assert_eq!(
        changed.mutation_from_previous.kind,
        CacheLayoutMutationKind::ToolSchemaChanged
    );
    assert!(!changed.mutation_from_previous.local_stable_prefix_preserved);
    Ok(())
}

#[test]
fn cache_layout_diagnostic_identifies_changed_message_fields_without_content() -> Result<()> {
    let baseline_request = request();
    let baseline = CacheLayoutProofV2::from_request(&baseline_request, None)?;
    let mut changed_request = baseline_request;
    changed_request.messages[1].role = MessageRole::Assistant;
    changed_request.messages[1].content = Some("rewritten answer".to_owned());
    let changed = CacheLayoutProofV2::from_request(&changed_request, Some(&baseline))?;

    let diagnostic = changed
        .mutation_from_previous
        .diagnostic
        .as_ref()
        .expect("rewrite should carry a bounded diagnostic");
    assert_eq!(diagnostic.first_changed_message_index, Some(0));
    assert!(
        diagnostic
            .changed_fields
            .contains(&crate::CacheLayoutMessageFieldV1::Role)
    );
    assert!(
        diagnostic
            .changed_fields
            .contains(&crate::CacheLayoutMessageFieldV1::Content)
    );
    assert!(
        diagnostic
            .changed_fields
            .contains(&crate::CacheLayoutMessageFieldV1::Size)
    );
    let rendered = serde_json::to_string(diagnostic)?;
    assert!(!rendered.contains("rewritten answer"));
    assert_eq!(
        diagnostic.previous_message_count,
        diagnostic.current_message_count
    );
    Ok(())
}

#[test]
fn cache_layout_rewrite_keeps_the_unchanged_prefix_count() -> Result<()> {
    let mut baseline_request = request();
    baseline_request.messages.extend([ModelMessage::assistant(
        Some("answer".to_owned()),
        Vec::new(),
    )]);
    let baseline = CacheLayoutProofV2::from_request(&baseline_request, None)?;
    let mut changed_request = baseline_request;
    changed_request.messages[2].content = Some("rewritten answer".to_owned());
    let changed = CacheLayoutProofV2::from_request(&changed_request, Some(&baseline))?;

    assert_eq!(
        changed
            .mutation_from_previous
            .reusable_conversation_message_count,
        1
    );
    assert_eq!(
        changed
            .mutation_from_previous
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.first_changed_message_index),
        Some(1)
    );
    Ok(())
}

#[test]
fn cache_layout_diagnostic_bounds_fingerprint_growth() -> Result<()> {
    let mut request = request();
    request
        .messages
        .extend((0..256).map(|index| ModelMessage::user(format!("message-{index}"))));
    let proof = CacheLayoutProofV2::from_request(&request, None)?;
    proof.validate()?;
    assert!(proof.mutation_from_previous.message_fingerprints.len() <= 128);
    assert!(
        proof
            .mutation_from_previous
            .diagnostic
            .as_ref()
            .is_some_and(|diagnostic| diagnostic.comparison_truncated)
    );
    Ok(())
}

#[test]
fn cache_layout_full_prefix_hash_survives_the_fingerprint_bound() -> Result<()> {
    let mut baseline_request = request();
    baseline_request
        .messages
        .extend((0..160).map(|index| ModelMessage::user(format!("message-{index}"))));
    let baseline = CacheLayoutProofV2::from_request(&baseline_request, None)?;
    let mut appended_request = baseline_request;
    appended_request
        .messages
        .push(ModelMessage::assistant(Some("tail".to_owned()), Vec::new()));
    let appended = CacheLayoutProofV2::from_request(&appended_request, Some(&baseline))?;

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
    Ok(())
}
