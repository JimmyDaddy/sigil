use std::fs;

use sigil_kernel::{JsonlSessionStore, ModelMessage, SessionLogEntry};

use super::*;
use crate::McpPluginTrustSource;

struct Fixture {
    root: tempfile::TempDir,
    session: Session,
    attachment: InteractiveSessionAttachmentLease,
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let plugin_root = root.path().join(".sigil/plugins/review");
        fs::create_dir_all(&plugin_root)?;
        fs::write(
            plugin_root.join("plugin.toml"),
            "id = \"review\"\nname = \"Review\"\nversion = \"1.0.0\"\n[[hooks]]\nid = \"check\"\nevent = \"verification\"\ncommand = \"must-not-run-during-review\"\n",
        )?;
        let path = root.path().join("session.jsonl");
        let attachment = InteractiveSessionAttachmentLease::acquire(&path)?;
        let mut session =
            Session::new("custom", "fixture").with_store(JsonlSessionStore::new(&path)?);
        session.append_control(ControlEntry::SessionIdentity {
            provider_name: "custom".to_owned(),
            model_name: "fixture".to_owned(),
            resolved_model_route: None,
        })?;
        session.append_user_message(ModelMessage::user("continue the existing task"))?;
        attachment.bind_application_operation_owner(&session)?;
        Ok(Self {
            root,
            session,
            attachment,
        })
    }

    fn request(&self, decision: PluginTrustDecision) -> Result<ApplicationPluginDecisionRequest> {
        let catalog = application_plugin_catalog(
            &self.attachment,
            self.session.session_scope_id(),
            self.root.path(),
        )?;
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        let snapshot = catalog.manifests.first().context("review snapshot")?;
        Ok(ApplicationPluginDecisionRequest {
            plugin_id: snapshot.plugin_id.clone(),
            expected_manifest_hash: snapshot.manifest_hash.clone(),
            expected_capability_digest: snapshot.capability_digest()?,
            decision,
        })
    }
}

#[test]
fn plugin_management_publishes_exact_trust_with_live_session_owner_and_no_process() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let before = fs::read(fixture.attachment.session_path())?;
    let request = fixture.request(PluginTrustDecision::Trusted)?;
    assert_eq!(
        fs::read(fixture.attachment.session_path())?,
        before,
        "query is observational"
    );
    let receipt = apply_application_plugin_decision(
        &fixture.attachment,
        fixture.session.session_scope_id(),
        fixture.root.path(),
        &request,
    )?;
    assert_eq!(receipt.decision, PluginTrustDecision::Trusted);
    // The original live writer remains usable; no idle or new controller acquisition is required.
    fixture
        .session
        .append_assistant_message(ModelMessage::assistant(
            Some("still running".to_owned()),
            Vec::new(),
        ))?;
    let disabled = fixture.request(PluginTrustDecision::Disabled)?;
    apply_application_plugin_decision(
        &fixture.attachment,
        fixture.session.session_scope_id(),
        fixture.root.path(),
        &disabled,
    )?;
    let source = crate::SessionMcpPluginTrustSource::new(fixture.attachment.session_path());
    assert_eq!(
        source.current_plugin_trust()?[0].decision,
        PluginTrustDecision::Disabled
    );
    let entries = JsonlSessionStore::read_entries(fixture.attachment.session_path())?;
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PluginTrustDecision(_))
            ))
            .count(),
        2
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(_))
            ))
            .count(),
        2
    );
    assert!(
        !fixture
            .root
            .path()
            .join("must-not-run-during-review")
            .exists()
    );
    Ok(())
}

#[test]
fn plugin_management_rejects_foreign_scope_and_stale_review_without_append() -> Result<()> {
    let fixture = Fixture::new()?;
    let request = fixture.request(PluginTrustDecision::Trusted)?;
    let before = fs::read(fixture.attachment.session_path())?;
    assert!(
        application_plugin_catalog(&fixture.attachment, "foreign", fixture.root.path()).is_err()
    );
    assert!(
        apply_application_plugin_decision(
            &fixture.attachment,
            "foreign",
            fixture.root.path(),
            &request
        )
        .is_err()
    );
    let mut bad_capability = request.clone();
    bad_capability.expected_capability_digest = "sha256:changed".to_owned();
    assert!(
        apply_application_plugin_decision(
            &fixture.attachment,
            fixture.session.session_scope_id(),
            fixture.root.path(),
            &bad_capability
        )
        .is_err()
    );
    let manifest = fixture
        .root
        .path()
        .join(".sigil/plugins/review/plugin.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)?.replace("Review", "Changed review"),
    )?;
    assert!(
        apply_application_plugin_decision(
            &fixture.attachment,
            fixture.session.session_scope_id(),
            fixture.root.path(),
            &request
        )
        .is_err()
    );
    assert_eq!(fs::read(fixture.attachment.session_path())?, before);
    Ok(())
}

#[test]
fn plugin_management_cannot_reconstruct_writer_from_an_unbound_attachment() -> Result<()> {
    let root = tempfile::tempdir()?;
    let attachment = InteractiveSessionAttachmentLease::acquire(root.path().join("empty.jsonl"))?;
    assert!(application_plugin_catalog(&attachment, "guessed-scope", root.path()).is_err());
    assert!(!attachment.session_path().exists());
    Ok(())
}

#[tokio::test]
async fn plugin_disable_retirement_uses_attested_scope_and_preserves_new_generation() -> Result<()>
{
    use sigil_kernel::{
        Tool, ToolAccess, ToolCategory, ToolContext, ToolLifecycleOwner, ToolPreviewCapability,
        ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Probe {
        name: String,
        owner: ToolLifecycleOwner,
        stops: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl Tool for Probe {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.clone(),
                description: "lifecycle fixture".to_owned(),
                input_schema: serde_json::json!({"type":"object"}),
                category: ToolCategory::Mcp,
                access: ToolAccess::Read,
                network_effect: None,
                preview: ToolPreviewCapability::None,
            }
        }
        fn lifecycle_owner(&self) -> Option<ToolLifecycleOwner> {
            Some(self.owner.clone())
        }
        async fn execute(
            &self,
            _ctx: ToolContext,
            id: String,
            _: serde_json::Value,
        ) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                id,
                &self.name,
                "fixture",
                ToolResultMeta::default(),
            ))
        }
        async fn shutdown(&self) -> Result<()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let fixture = Fixture::new()?;
    let manifest_path = fixture
        .root
        .path()
        .join(".sigil/plugins/review/plugin.toml");
    fs::write(
        &manifest_path,
        format!(
            "{}\n[[mcp_servers]]\ntransport = \"stdio\"\nname = \"echo\"\ncommand = \"python3\"\nstartup = \"lazy\"\n",
            fs::read_to_string(&manifest_path)?
        ),
    )?;
    let trusted = fixture.request(PluginTrustDecision::Trusted)?;
    apply_application_plugin_decision(
        &fixture.attachment,
        fixture.session.session_scope_id(),
        fixture.root.path(),
        &trusted,
    )?;
    let mut config: sigil_kernel::RootConfig =
        toml::from_str("config_version = 2\n[agent]\nconnection = 'fixture'\nmodel = 'fixture'\n")?;
    config.mcp_servers.push(toml::from_str(
        "transport = 'stdio'\nname = 'review.echo'\ncommand = 'python3'\nstartup = 'lazy'\n",
    )?);
    let source = Arc::new(crate::SessionMcpPluginTrustSource::new(
        fixture.attachment.session_path(),
    ));
    let report =
        crate::discover_workspace_plugins(fixture.root.path(), &source.current_plugin_trust()?)?;
    let declarations = crate::merge_mcp_server_declarations(
        &crate::resolve_user_root_mcp_declarations(&config.mcp_servers, fixture.root.path())?,
        &report.registrations.mcp_servers,
    )?;
    let scope = declarations
        .iter()
        .find(|item| matches!(item.origin(), crate::McpConfigOrigin::PluginManifest { .. }))
        .context("plugin declaration")?
        .effective_name();
    assert_ne!(
        scope, "review.echo",
        "user declaration wins collision and stays separate"
    );
    let mut registry = ToolRegistry::new();
    let stops = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    for (name, scope, generation, counter) in [
        (
            "user",
            "review.echo",
            "user-generation",
            Arc::clone(&stops.0),
        ),
        ("plugin-old", scope, "old-generation", Arc::clone(&stops.1)),
    ] {
        let mut owner =
            ToolLifecycleOwner::new(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE, scope, generation);
        if name == "plugin-old" {
            owner = owner.with_origin(sigil_kernel::ToolLifecycleOrigin {
                namespace: "plugin".to_owned(),
                subject: "review".to_owned(),
                revision: trusted.expected_manifest_hash.clone(),
            });
        }
        registry.register(Arc::new(Probe {
            name: name.to_owned(),
            owner,
            stops: counter,
        }));
    }
    // A changed current manifest cannot erase the immutable origin of the already-launched owner.
    fs::write(
        &manifest_path,
        fs::read_to_string(&manifest_path)?.replace("1.0.0", "2.0.0"),
    )?;
    let retirement = crate::prepare_plugin_mcp_retirement(&registry, &trusted.plugin_id);
    let disabled = fixture.request(PluginTrustDecision::Disabled)?;
    apply_application_plugin_decision(
        &fixture.attachment,
        fixture.session.session_scope_id(),
        fixture.root.path(),
        &disabled,
    )?;
    let reenabled = fixture.request(PluginTrustDecision::Trusted)?;
    apply_application_plugin_decision(
        &fixture.attachment,
        fixture.session.session_scope_id(),
        fixture.root.path(),
        &reenabled,
    )?;
    registry.register(Arc::new(Probe {
        name: "plugin-new".to_owned(),
        owner: ToolLifecycleOwner::new(
            sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE,
            scope,
            "new-generation",
        )
        .with_origin(sigil_kernel::ToolLifecycleOrigin {
            namespace: "plugin".to_owned(),
            subject: "review".to_owned(),
            revision: reenabled.expected_manifest_hash,
        }),
        stops: Arc::clone(&stops.2),
    }));
    retirement.settle().await?;
    assert_eq!(stops.0.load(Ordering::SeqCst), 0);
    assert_eq!(stops.1.load(Ordering::SeqCst), 1);
    assert_eq!(stops.2.load(Ordering::SeqCst), 0);
    assert!(registry.specs().iter().any(|spec| spec.name == "user"));
    assert!(
        registry
            .specs()
            .iter()
            .any(|spec| spec.name == "plugin-new")
    );
    assert!(
        !registry
            .specs()
            .iter()
            .any(|spec| spec.name == "plugin-old")
    );
    Ok(())
}

#[tokio::test]
async fn plugin_review_requires_its_exact_prepared_operation_for_commit_proof() -> Result<()> {
    use sigil_kernel::{ApplicationOperationBindingV1, ApplicationOperationTargetV1};
    let fixture = Fixture::new()?;
    let request = fixture.request(PluginTrustDecision::Trusted)?;
    let owner = fixture
        .attachment
        .application_operation_owner()
        .context("owner")?;
    let binding = ApplicationOperationBindingV1::new(
        fixture.session.session_scope_id().to_owned(),
        "1".repeat(64),
        "2".repeat(64),
        ApplicationOperationTargetV1::ReviewPlugin {
            plugin_id: request.plugin_id.clone(),
            manifest_hash: request.expected_manifest_hash.clone(),
            capability_digest: request.expected_capability_digest.clone(),
            decision: request.decision,
        },
    )?;
    // Equal historical review is not this command's proof.
    apply_application_plugin_decision_with_owner(
        &owner,
        fixture.session.session_scope_id(),
        fixture.root.path(),
        &request,
    )?;
    owner.prepare(&binding)?;
    assert!(
        sigil_kernel::session::reconcile_application_operation(&owner.read_handle(), &binding)?
            .is_none()
    );
    let mut controlled = owner.attach_for_control()?;
    controlled.bind_application_operation(binding.clone())?;
    let publication = apply_application_plugin_decision_to_session(
        &mut controlled,
        fixture.session.session_scope_id(),
        fixture.root.path(),
        &request,
    );
    assert!(
        sigil_kernel::session::reconcile_application_operation(&owner.read_handle(), &binding)?
            .is_none(),
        "trust publication alone cannot commit the complete review operation"
    );
    let (receipt, _, _) =
        settle_application_plugin_decision(&mut controlled, publication, Vec::new()).await?;
    let proof =
        sigil_kernel::session::reconcile_application_operation(&owner.read_handle(), &binding)?
            .context("post-review proof")?;
    assert!(
        matches!(proof.matched_control(), ControlEntry::PluginReviewCompletedV1(result)
        if result.trust_event_id == receipt.trust_event_id && result.process_cleanup.is_none())
    );
    let _ = controlled.clear_application_operation();
    let foreign = ApplicationOperationBindingV1::new(
        fixture.session.session_scope_id().to_owned(),
        "3".repeat(64),
        "4".repeat(64),
        ApplicationOperationTargetV1::ReviewPlugin {
            plugin_id: request.plugin_id.clone(),
            manifest_hash: request.expected_manifest_hash.clone(),
            capability_digest: "sha256:wrong-capability".to_owned(),
            decision: request.decision,
        },
    )?;
    owner.prepare(&foreign)?;
    controlled.bind_application_operation(foreign.clone())?;
    let before = fs::read(fixture.attachment.session_path())?;
    assert!(
        apply_application_plugin_decision_to_session(
            &mut controlled,
            fixture.session.session_scope_id(),
            fixture.root.path(),
            &request
        )
        .is_err()
    );
    assert_eq!(fs::read(fixture.attachment.session_path())?, before);
    assert!(
        sigil_kernel::session::reconcile_application_operation(&owner.read_handle(), &foreign)?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn plugin_cleanup_missing_owner_and_late_receipt_cannot_erase_unknown() -> Result<()> {
    let fixture = Fixture::new()?;
    let owner = fixture
        .attachment
        .application_operation_owner()
        .context("owner")?;
    let mut session = owner.attach_for_control()?;
    let scope = session.session_scope_id().to_owned();
    let disabled = fixture.request(PluginTrustDecision::Disabled)?;
    let first = apply_application_plugin_decision_to_session(
        &mut session,
        &scope,
        fixture.root.path(),
        &disabled,
    )?;
    // Model restart/lost owner: a durable disable without its post-review result survives.
    let retry = apply_application_plugin_decision_to_session(
        &mut session,
        &scope,
        fixture.root.path(),
        &disabled,
    );
    let (retry, _, _) = settle_application_plugin_decision(&mut session, retry, Vec::new()).await?;
    assert_eq!(
        retry.process_cleanup,
        Some(sigil_kernel::PluginCleanupStatus::Unknown)
    );
    // A delayed result for the first event cannot overwrite the new operation's Unknown.
    session.append_control(ControlEntry::PluginReviewCompletedV1(
        sigil_kernel::PluginReviewCompletedV1 {
            plugin_id: first.0.plugin_id,
            manifest_hash: first.0.manifest_hash,
            capability_digest: first.0.capability_digest,
            decision: first.0.decision,
            trust_event_id: first.0.trust_event_id,
            process_cleanup: Some(sigil_kernel::PluginCleanupStatus::Confirmed),
        },
    ))?;
    session.append_durable_event(sigil_kernel::DurableEventType::ExtensionProcessLifecycleRecorded,
        sigil_kernel::EventClass::Critical, serde_json::json!({
            "process_kind":"mcp_stdio", "subject":"review.unrelated", "phase":"post_spawn", "status":"stopped",
            "safe_metadata":{"mcp_config_origin":"plugin", "mcp_config_origin_id":"review", "process_generation":"later-closed"}
        }))?;
    let retry = apply_application_plugin_decision_to_session(
        &mut session,
        &scope,
        fixture.root.path(),
        &disabled,
    );
    let (retry, _, _) = settle_application_plugin_decision(&mut session, retry, Vec::new()).await?;
    assert_eq!(
        retry.process_cleanup,
        Some(sigil_kernel::PluginCleanupStatus::Unknown),
        "an unrelated closed generation does not explain the earlier lost owner"
    );
    let enabled = fixture.request(PluginTrustDecision::Trusted)?;
    let publication = apply_application_plugin_decision_to_session(
        &mut session,
        &scope,
        fixture.root.path(),
        &enabled,
    );
    let (enabled, _, _) =
        settle_application_plugin_decision(&mut session, publication, Vec::new()).await?;
    assert_eq!(
        enabled.process_cleanup,
        Some(sigil_kernel::PluginCleanupStatus::Unknown)
    );
    let catalog = application_plugin_catalog(&fixture.attachment, &scope, fixture.root.path())?;
    assert_eq!(
        catalog.process_cleanup.get("review"),
        Some(&sigil_kernel::PluginCleanupStatus::Unknown)
    );
    Ok(())
}

#[tokio::test]
async fn plugin_cleanup_recovers_only_from_exact_generation_stop_evidence() -> Result<()> {
    use sigil_kernel::{DurableEventType, EventClass};
    let fixture = Fixture::new()?;
    let owner = fixture
        .attachment
        .application_operation_owner()
        .context("owner")?;
    let mut session = owner.attach_for_control()?;
    let scope = session.session_scope_id().to_owned();
    let audit = |generation: &str, status: &str| {
        serde_json::json!({
            "process_kind":"mcp_stdio", "subject":"review.server", "phase":"post_spawn", "status":status,
            "safe_metadata":{"mcp_config_origin":"plugin", "mcp_config_origin_id":"review", "process_generation":generation}
        })
    };
    session.append_durable_event(
        DurableEventType::ExtensionProcessLifecycleRecorded,
        EventClass::Critical,
        audit("old", "running"),
    )?;
    let disabled = fixture.request(PluginTrustDecision::Disabled)?;
    let publication = apply_application_plugin_decision_to_session(
        &mut session,
        &scope,
        fixture.root.path(),
        &disabled,
    );
    session.append_durable_event(
        DurableEventType::ExtensionProcessLifecycleRecorded,
        EventClass::Critical,
        audit("other", "stopped"),
    )?;
    let (unknown, _, _) =
        settle_application_plugin_decision(&mut session, publication, Vec::new()).await?;
    assert_eq!(
        unknown.process_cleanup,
        Some(sigil_kernel::PluginCleanupStatus::Unknown)
    );
    session.append_durable_event(
        DurableEventType::ExtensionProcessLifecycleRecorded,
        EventClass::Critical,
        audit("old", "stop_unconfirmed"),
    )?;
    let publication = apply_application_plugin_decision_to_session(
        &mut session,
        &scope,
        fixture.root.path(),
        &disabled,
    );
    let (failed, _, _) =
        settle_application_plugin_decision(&mut session, publication, Vec::new()).await?;
    assert_eq!(
        failed.process_cleanup,
        Some(sigil_kernel::PluginCleanupStatus::Unconfirmed)
    );
    session.append_durable_event(
        DurableEventType::ExtensionProcessLifecycleRecorded,
        EventClass::Critical,
        audit("old", "stopped"),
    )?;
    let publication = apply_application_plugin_decision_to_session(
        &mut session,
        &scope,
        fixture.root.path(),
        &disabled,
    );
    let (confirmed, _, _) =
        settle_application_plugin_decision(&mut session, publication, Vec::new()).await?;
    assert_eq!(
        confirmed.process_cleanup,
        Some(sigil_kernel::PluginCleanupStatus::Confirmed)
    );
    Ok(())
}

#[tokio::test]
async fn plugin_cleanup_failure_joins_all_captures_and_commits_unconfirmed_proof() -> Result<()> {
    use sigil_kernel::{
        ApplicationOperationBindingV1, ApplicationOperationTargetV1, Tool, ToolAccess,
        ToolCategory, ToolContext, ToolPreviewCapability, ToolRegistry, ToolResult, ToolSpec,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct CleanupProbe {
        fail: bool,
        joins: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl Tool for CleanupProbe {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "cleanup_probe".to_owned(),
                description: "owned cleanup fault injection".to_owned(),
                input_schema: serde_json::json!({"type":"object"}),
                category: ToolCategory::Mcp,
                access: ToolAccess::Read,
                network_effect: None,
                preview: ToolPreviewCapability::None,
            }
        }
        async fn execute(
            &self,
            _: ToolContext,
            _: String,
            _: serde_json::Value,
        ) -> Result<ToolResult> {
            anyhow::bail!("cleanup probe is never dispatched")
        }
        fn prepare_background_work_retirement(
            &self,
            namespace: &str,
            subject: &str,
        ) -> Vec<futures::future::BoxFuture<'static, Result<()>>> {
            assert_eq!((namespace, subject), ("plugin", "review"));
            let joins = Arc::clone(&self.joins);
            let fail = self.fail;
            vec![Box::pin(async move {
                joins.fetch_add(1, Ordering::SeqCst);
                anyhow::ensure!(!fail, "fixture cannot confirm its process exit");
                Ok(())
            })]
        }
    }
    let fixture = Fixture::new()?;
    let owner = fixture
        .attachment
        .application_operation_owner()
        .context("owner")?;
    let mut session = owner.attach_for_control()?;
    let scope = session.session_scope_id().to_owned();
    let request = fixture.request(PluginTrustDecision::Disabled)?;
    let binding = ApplicationOperationBindingV1::new(
        scope.clone(),
        "5".repeat(64),
        "6".repeat(64),
        ApplicationOperationTargetV1::ReviewPlugin {
            plugin_id: request.plugin_id.clone(),
            manifest_hash: request.expected_manifest_hash.clone(),
            capability_digest: request.expected_capability_digest.clone(),
            decision: request.decision,
        },
    )?;
    owner.prepare(&binding)?;
    session.bind_application_operation(binding.clone())?;
    let joins = Arc::new(AtomicUsize::new(0));
    let retirements = [true, false]
        .into_iter()
        .map(|fail| {
            let mut registry = ToolRegistry::new();
            registry.register(Arc::new(CleanupProbe {
                fail,
                joins: Arc::clone(&joins),
            }));
            crate::prepare_plugin_mcp_retirement(&registry, "review")
        })
        .collect();
    let publication = apply_application_plugin_decision_to_session(
        &mut session,
        &scope,
        fixture.root.path(),
        &request,
    );
    let (receipt, _, error) =
        settle_application_plugin_decision(&mut session, publication, retirements).await?;
    assert_eq!(
        joins.load(Ordering::SeqCst),
        2,
        "failure must not skip another captured owner"
    );
    assert!(error.is_some());
    assert_eq!(
        receipt.process_cleanup,
        Some(sigil_kernel::PluginCleanupStatus::Unconfirmed)
    );
    let proof =
        sigil_kernel::session::reconcile_application_operation(&owner.read_handle(), &binding)?
            .context("complete result proof")?;
    assert!(
        matches!(proof.matched_control(), ControlEntry::PluginReviewCompletedV1(result)
        if result.process_cleanup == Some(sigil_kernel::PluginCleanupStatus::Unconfirmed))
    );
    assert_eq!(
        application_plugin_catalog(&fixture.attachment, &scope, fixture.root.path())?
            .process_cleanup
            .get("review"),
        Some(&sigil_kernel::PluginCleanupStatus::Unconfirmed)
    );
    Ok(())
}
