use super::*;

struct ExplicitNetworkApproval(ApproveForSessionHandler);
impl ApprovalHandler for ExplicitNetworkApproval {
    fn approve_tool_call(
        &mut self,
        call: &ToolCall,
        spec: &crate::ToolSpec,
    ) -> Result<ToolApproval> {
        self.0.approve_tool_call(call, spec)
    }
    fn approval_is_explicit_user_action(&self) -> bool {
        true
    }
}

struct NetworkReadTool {
    executions: Arc<AtomicUsize>,
    binding: Arc<Mutex<BTreeMap<String, String>>>,
}

struct NetworkGrantProvider(AtomicUsize);

#[async_trait]
impl Provider for NetworkGrantProvider {
    fn name(&self) -> &str {
        "network-grant-fixture"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        WriteMockProvider.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let index = self.0.fetch_add(1, Ordering::SeqCst);
        let chunks = if index.is_multiple_of(2) {
            vec![
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: format!("network-call-{index}"),
                    name: "read_path".to_owned(),
                    args_json: "{}".to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ]
        } else {
            vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[async_trait]
impl Tool for NetworkReadTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "read_path".to_owned(),
            description: "network read fixture".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::Search,
            access: ToolAccess::Read,
            network_effect: Some(crate::NetworkEffect::Read),
            preview: ToolPreviewCapability::None,
        }
    }
    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        Ok(crate::ToolPermissionPlanDraft {
            access: ToolAccess::Read,
            operation: crate::ToolOperation::NetworkRequest,
            effects: BTreeSet::from([crate::ToolPermissionEffect::NetworkRead]),
            subjects: vec![ToolSubject {
                kind: crate::ToolSubjectKind::NetworkEndpoint,
                original: "https://example.test/page?[redacted]".to_owned(),
                normalized: "https://example.test/page?[redacted]".to_owned(),
                canonical_path: None,
                scope: ToolSubjectScope::External,
                access: ToolAccess::Read,
            }],
            analysis: crate::ToolAnalysisStatus::Complete,
            containment: Default::default(),
            semantic_scope: Some(crate::ToolSemanticScope::new("network_read", 1)),
            tool_default_mode: None,
            managed_file_access: None,
            analysis_bindings: self.binding.lock().expect("binding").clone(),
            safe_summary: crate::ToolPermissionSummary {
                title: "Read network endpoint".to_owned(),
                detail: "network fixture".to_owned(),
                ..Default::default()
            },
        })
    }
    async fn execute(&self, ctx: ToolContext, call_id: String, _args: Value) -> Result<ToolResult> {
        assert!(ctx.explicit_network_approval());
        assert!(
            ctx.prepared_permission_plan()
                .expect("approved plan")
                .session_grant_containment_binding()
                .is_none()
        );
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            "read_path",
            "read",
            ToolResultMeta::default(),
        ))
    }
}

#[tokio::test]
async fn network_session_grant_executes_reloads_and_reuses_without_shell_containment() -> Result<()>
{
    let workspace = tempfile::tempdir()?;
    let path = workspace.path().join("session.jsonl");
    let executions = Arc::new(AtomicUsize::new(0));
    let approvals = Arc::new(AtomicUsize::new(0));
    let binding = Arc::new(Mutex::new(BTreeMap::from_iter(
        [
            "network_endpoint_hash",
            "network_transport_hash",
            "network_route_hash",
            "network_policy_hash",
        ]
        .map(|key| (key.to_owned(), crate::sha256_hex(key.as_bytes()))),
    )));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(NetworkReadTool {
        executions: executions.clone(),
        binding: binding.clone(),
    }));
    let agent = Agent::new(NetworkGrantProvider(AtomicUsize::new(0)), registry.clone());
    let run_options = || AgentRunOptions {
        workspace_root: workspace.path().to_path_buf(),
        max_turns: Some(4),
        tool_timeout_secs: 5,
        reasoning_effort: None,
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_mode_override: None,
        permission_context: crate::PermissionEvaluationContext {
            network_policy: crate::NetworkPolicy::Ask,
            ..Default::default()
        },
        memory_config: MemoryConfig::with_enabled(false),
        compaction_config: CompactionConfig::default(),
        tool_authority: None,
    };
    let mut session =
        Session::new("network-grant", "mock-model").with_store(JsonlSessionStore::new(&path)?);
    session.ensure_identity_entry()?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = ExplicitNetworkApproval(ApproveForSessionHandler {
        approvals: approvals.clone(),
    });
    agent
        .run_with_approval(
            &mut session,
            "first read",
            run_options(),
            &mut handler,
            &mut approval,
        )
        .await?;
    assert_eq!(
        approvals.load(Ordering::SeqCst),
        1,
        "events: {:?}; entries: {:?}",
        handler.events,
        session.entries()
    );
    let grant = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ToolApprovalSessionGrant(grant)) => {
                Some(grant.clone())
            }
            _ => None,
        })
        .expect("durable grant");
    assert!(grant.containment_binding.is_none());
    assert!(grant.network_binding.is_some());
    grant.validate()?;
    drop(session);
    let mut session = Session::load_from_store(
        "network-grant",
        "mock-model",
        JsonlSessionStore::new(&path)?,
    )?;
    agent
        .run_with_approval(
            &mut session,
            "second read",
            run_options(),
            &mut handler,
            &mut approval,
        )
        .await?;
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
    // Same redacted display URL, different exact endpoint/route/transport/policy must prompt.
    for (index, key) in [
        "network_endpoint_hash",
        "network_transport_hash",
        "network_route_hash",
        "network_policy_hash",
    ]
    .into_iter()
    .enumerate()
    {
        binding.lock().expect("binding").insert(
            key.to_owned(),
            crate::sha256_hex(format!("changed-{index}").as_bytes()),
        );
        agent
            .run_with_approval(
                &mut session,
                "changed network binding",
                run_options(),
                &mut handler,
                &mut approval,
            )
            .await?;
        assert_eq!(approvals.load(Ordering::SeqCst), index + 2);
    }
    let context = ToolContext::new(workspace.path().to_path_buf(), 5);
    let plan = registry.permission_plan(
        &context,
        &ToolCall {
            id: "read".to_owned(),
            name: "read_path".to_owned(),
            args_json: "{}".to_owned(),
        },
    )?;
    let options = run_options();
    let policy = crate::PermissionPolicyChain::new_with_context(
        &options.permission_config,
        &options.permission_context,
    );
    let decision = policy.decide_plan(&registry.spec_for("read_path").expect("spec"), &plan)?;
    assert!(
        !super::super::approval_policy::session_grant_covers_decision(
            &grant,
            &plan,
            "changed-authority-policy",
            &decision
        )
    );
    Ok(())
}

#[test]
fn local_session_grant_json_remains_readable_and_mixed_bindings_are_rejected() -> Result<()> {
    let legacy = json!({
        "schema_version": crate::TOOL_APPROVAL_SESSION_GRANT_SCHEMA_VERSION,
        "grant_id":"grant", "source_call_id":"call", "source_approval_request_id":"approval",
        "tool_name":"read", "semantic_scope":{"family":"read", "version":1},
        "effect_ceiling":["file_read"], "risk_ceiling":"low", "subjects":[], "facets":["local"],
        "scope":"exact_subjects", "containment_binding": {
            "requested": crate::ExecutionContainmentRequest::default(),
            "backend_identity_hash": crate::sha256_hex(b"backend"), "backend_profile_hash": crate::sha256_hex(b"profile"), "environment_binding_hash": crate::sha256_hex(b"environment")
        }, "policy_version":"policy", "expires":"session", "granted_at_ms":1
    });
    let mut grant: crate::ToolApprovalSessionGrantEntry = serde_json::from_value(legacy.clone())?;
    grant.validate()?;
    assert_eq!(serde_json::to_value(&grant)?, legacy);
    grant.network_binding = Some(crate::NetworkSessionGrantBindingV1 {
        endpoint_hash: crate::sha256_hex(b"endpoint"),
        transport_hash: crate::sha256_hex(b"transport"),
        route_hash: crate::sha256_hex(b"route"),
        policy_hash: crate::sha256_hex(b"policy"),
    });
    assert!(grant.validate().is_err());
    Ok(())
}
