use std::{
    fs,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use sigil_kernel::{
    PluginTrustDecision, PluginTrustEntry, ToolCall, ToolRegistryScope, ToolResultStatus,
    resource::{AuthorityGeneration, CanonicalHash},
};

use super::*;

struct TrustSource {
    entries: Mutex<Vec<PluginTrustEntry>>,
    reads: AtomicUsize,
    revoke_at: usize,
}

impl McpPluginTrustSource for TrustSource {
    fn current_plugin_trust(&self) -> Result<Vec<PluginTrustEntry>> {
        let read = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        let entries = self.entries.lock().expect("trust fixture lock");
        if read == self.revoke_at {
            return Ok(Vec::new());
        }
        Ok(entries.clone())
    }
}

struct WorkflowFixture {
    directory: tempfile::TempDir,
    source: Arc<TrustSource>,
    composition: crate::r71_authority_composition::RuntimeAuthorityCompositionV1,
}

impl WorkflowFixture {
    fn new(revoke_at: usize) -> Result<Self> {
        Self::with_command(revoke_at, "printf actual-reviewed-hook")
    }

    fn with_command(revoke_at: usize, command: &str) -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let plugin_root = directory.path().join(".sigil/plugins/review");
        fs::create_dir_all(&plugin_root)?;
        fs::write(
            plugin_root.join("plugin.toml"),
            r#"id = "review"
name = "Reviewed workflow"
version = "1.0.0"
[[hooks]]
id = "check"
event = "verification"
kind = "verification"
command = "/bin/sh"
args = ["-c", "printf actual-reviewed-hook"]
declared_effect = "read_only"
approval = "ask"
timeout_ms = 5000
"#
            .replace(
                "\"printf actual-reviewed-hook\"",
                &serde_json::to_string(command)?,
            ),
        )?;
        let discovered = discover_workspace_plugins(directory.path(), &[])?;
        let trust = PluginTrustEntry::for_snapshot(
            &discovered.manifests[0],
            PluginTrustDecision::Trusted,
            42,
        )?;
        let source = Arc::new(TrustSource {
            entries: Mutex::new(vec![trust]),
            reads: AtomicUsize::new(0),
            revoke_at,
        });
        let state = directory.path().join("state");
        let scratch = directory.path().join("execution-temp");
        fs::create_dir_all(state.join("cache"))?;
        fs::create_dir_all(&scratch)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
            fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700))?;
        }
        let planner = Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        ));
        let composition =
            crate::r71_authority_composition::compose_runtime_authority_for_test_execution(
                &state,
                &scratch,
                CanonicalHash::from_bytes([0x83; 32]),
                AuthorityGeneration {
                    epoch: 1,
                    instance_hash: CanonicalHash::from_bytes([0x84; 32]),
                },
                planner,
                &[crate::managed_storage_writer::StorageWriterChannelV1::ApplicationControlLog],
            )?;
        Ok(Self {
            directory,
            source,
            composition,
        })
    }

    async fn registry(&self) -> Result<ToolRegistry> {
        let mut registry = ToolRegistry::new();
        let source: Arc<dyn McpPluginTrustSource> = self.source.clone();
        let executor: Arc<dyn ManagedPluginHookExecutionPortV1> = self
            .composition
            .plugin_hook_execution
            .clone()
            .expect("real hook route");
        let warnings = register_plugin_workflow_tools(
            &mut registry,
            self.directory.path(),
            source,
            executor,
            SecretRedactor::default(),
        )
        .await?;
        assert!(warnings.is_empty(), "{warnings:?}");
        Ok(registry)
    }

    fn records(&self) -> Result<String> {
        let root = self
            .directory
            .path()
            .join("state/managed/application-control-log");
        if !root.exists() {
            return Ok(String::new());
        }
        let mut records = String::new();
        for entry in fs::read_dir(root)? {
            let file = entry?.path().join("records.jsonl");
            if file.exists() {
                records.push_str(&fs::read_to_string(file)?);
            }
        }
        Ok(records)
    }

    fn call(&self, registry: &ToolRegistry) -> (ToolContext, ToolCall) {
        let spec = registry
            .specs()
            .into_iter()
            .next()
            .expect("trusted hook spec");
        let context = ToolContext::new(self.directory.path(), 5);
        let call = ToolCall {
            id: "explicit-review-check".to_owned(),
            name: spec.name,
            args_json: "{}".to_owned(),
        };
        let plan = registry
            .permission_plan(&context, &call)
            .expect("hook permission plan");
        (context.with_approved_subjects(plan.subjects), call)
    }
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_workflow_executes_reviewed_ask_hook_via_real_extension_route() -> Result<()> {
    let fixture = WorkflowFixture::new(usize::MAX)?;
    let registry = fixture.registry().await?;
    assert_eq!(registry.specs().len(), 1);
    assert!(
        fixture.records()?.is_empty(),
        "discovery must not launch or authorize a process"
    );
    let (context, call) = fixture.call(&registry);
    let result = registry.execute(context, call).await?;
    assert_eq!(result.status, ToolResultStatus::Ok);
    assert_eq!(result.metadata.exit_code, Some(0));
    assert!(result.content.contains("actual-reviewed-hook"));
    assert!(matches!(
        &result.control_entries[0],
        ControlEntry::PluginHookExecutionStarted(_)
    ));
    assert!(
        matches!(&result.control_entries[1], ControlEntry::PluginHookExecutionFinished(entry) if entry.status == sigil_kernel::PluginHookExecutionStatus::Succeeded)
    );
    let records = fixture.records()?;
    assert!(records.contains("extension_start_authorized"));
    assert!(records.contains("extension_process_settled"));
    assert_eq!(
        fixture.source.reads.load(Ordering::SeqCst),
        3,
        "discovery, execution and immediately before actual spawn each read current trust"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_workflow_rejects_unapproved_hidden_drifted_or_disabled_hook() -> Result<()> {
    let fixture = WorkflowFixture::new(usize::MAX)?;
    let registry = fixture.registry().await?;
    let (_, call) = fixture.call(&registry);
    let error = registry
        .execute(ToolContext::new(fixture.directory.path(), 5), call.clone())
        .await
        .expect_err("Ask needs this exact registration's approval");
    assert!(error.to_string().contains("approval subject"));
    let scoped = registry.scoped(ToolRegistryScope::default());
    assert!(scoped.specs().is_empty());
    assert!(
        scoped
            .execute(ToolContext::new(fixture.directory.path(), 5), call.clone())
            .await
            .is_err()
    );
    let (context, _) = fixture.call(&registry);
    fixture
        .source
        .entries
        .lock()
        .expect("trust fixture lock")
        .clear();
    assert!(
        registry.execute(context, call.clone()).await.is_err(),
        "a revoked old registration cannot execute"
    );
    assert!(
        fixture.registry().await?.specs().is_empty(),
        "a disabled hook is not offered after reassembly"
    );
    assert!(fixture.records()?.is_empty());

    let fixture = WorkflowFixture::new(usize::MAX)?;
    let registry = fixture.registry().await?;
    let (context, call) = fixture.call(&registry);
    let path = fixture
        .directory
        .path()
        .join(".sigil/plugins/review/plugin.toml");
    let manifest = fs::read_to_string(&path)?;
    fs::write(
        path,
        manifest.replace("actual-reviewed-hook", "unreviewed-hook"),
    )?;
    assert!(registry.execute(context, call).await.is_err());
    assert!(
        fixture.records()?.is_empty(),
        "changed manifest must fail before authority preparation"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_workflow_rechecks_trust_after_authorization_before_real_spawn() -> Result<()> {
    let fixture = WorkflowFixture::new(3)?;
    let registry = fixture.registry().await?;
    let (context, call) = fixture.call(&registry);
    assert!(registry.execute(context, call).await.is_err());
    let records = fixture.records()?;
    assert!(
        records.contains("extension_start_authorized"),
        "exercise the actual managed route after preparation"
    );
    assert!(records.contains("extension_start_rejected"));
    assert!(
        !records.contains("extension_process_settled"),
        "revocation must prevent a physical process from being admitted"
    );
    Ok(())
}

#[derive(Debug)]
struct GateBeforeSpawn {
    calls: AtomicUsize,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl PluginHookPreSpawnCheck for GateBeforeSpawn {
    fn validate_current(&self) -> Result<()> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            if let Some(entered) = self.entered.lock().expect("gate lock").take() {
                let _ = entered.send(());
            }
            // Closing the sole sender releases this actual call during early test unwinding too.
            let _ = self.release.lock().expect("gate lock").recv();
        }
        Ok(())
    }
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_workflow_cancel_during_fresh_check_never_launches_marker_command() -> Result<()> {
    let fixture = WorkflowFixture::with_command(
        usize::MAX,
        "printf forbidden > \"$SIGIL_WORKSPACE_ROOT/late-hook-marker\"",
    )?;
    let registry = fixture.registry().await?;
    let (context, _) = fixture.call(&registry);
    let subject = context.approved_subjects()[0].clone();
    let trust = fixture.source.current_plugin_trust()?;
    let registration = discover_workspace_plugins(fixture.directory.path(), &trust)?
        .registrations
        .hooks
        .remove(0);
    let (entered, wait_entered) = tokio::sync::oneshot::channel();
    let (release, wait_release) = std::sync::mpsc::channel();
    let gate = Arc::new(GateBeforeSpawn {
        calls: AtomicUsize::new(0),
        entered: Mutex::new(Some(entered)),
        release: Mutex::new(wait_release),
    });
    let owner = sigil_kernel::RunCancellationOwner::new();
    let mut request =
        PluginHookExecutionRequest::new(registration, fixture.directory.path().to_path_buf())
            .with_cancellation(owner.handle());
    request.pre_spawn_check = Some(gate);
    let runner = fixture
        .composition
        .plugin_hook_runner()
        .expect("real hook route");
    let execution = tokio::spawn(async move {
        runner
            .execute_after_tool_approval(request, &context, &subject)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), wait_entered).await??;
    assert!(owner.request_cancel());
    drop(release);
    let error = execution
        .await?
        .expect_err("cancelled pre-spawn command must reject");
    assert!(format!("{error:#}").contains("cancelled before process launch"));
    assert!(!fixture.directory.path().join("late-hook-marker").exists());
    let records = fixture.records()?;
    assert!(records.contains("extension_start_authorized"));
    assert!(records.contains("extension_start_rejected"));
    assert!(!records.contains("extension_process_settled"));
    Ok(())
}

struct WorkflowMcpDisclosurePresenter;

#[async_trait]
impl sigil_kernel::EgressDisclosurePresenter for WorkflowMcpDisclosurePresenter {
    async fn present(
        &self,
        disclosure: sigil_kernel::PreEgressDisclosure,
    ) -> std::result::Result<
        sigil_kernel::DisclosurePresentationReceipt,
        sigil_kernel::DisclosurePresentationError,
    > {
        disclosure.presentation_receipt("isolated-plugin-workflow-test")
    }
}

#[cfg(unix)]
async fn execute_reviewed_mcp_with_started_audit(
    registry: &ToolRegistry,
    store: &sigil_kernel::JsonlSessionStore,
    context: ToolContext,
    call: ToolCall,
) -> Result<sigil_kernel::ToolResult> {
    let mut started = sigil_kernel::durable_tool_execution_entry(
        &call,
        context.approved_subjects(),
        sigil_kernel::ToolExecutionStatus::Started,
        None,
        None,
    )?;
    let profile = registry
        .execution_mutation_profile(&context, &call)?
        .ok_or_else(|| anyhow::anyhow!("reviewed MCP call needs an execution profile"))?;
    started.metadata.details["execution_mutation_profile"] = serde_json::to_value(profile)?;
    store.append(&sigil_kernel::SessionLogEntry::Control(
        sigil_kernel::ControlEntry::ToolExecution(Box::new(started)),
    ))?;
    let result = registry
        .execute_after_started_audit(context.clone(), call.clone())
        .await?;
    let finished = sigil_kernel::durable_tool_execution_entry(
        &call,
        context.approved_subjects(),
        if result.is_error() {
            sigil_kernel::ToolExecutionStatus::Failed
        } else {
            sigil_kernel::ToolExecutionStatus::Completed
        },
        Some(0),
        Some(&result),
    )?;
    store.append(&sigil_kernel::SessionLogEntry::Control(
        sigil_kernel::ControlEntry::ToolExecution(Box::new(finished)),
    ))?;
    Ok(result)
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_mcp_catalog_activates_reviewed_declaration_and_rechecks_each_call() -> Result<()> {
    // Both invalidations happen after a successful call and after preparing the next call.
    // The old registration must not forward either stale request to the still-live server.
    for change_manifest in [false, true] {
        let fixture = WorkflowFixture::new(usize::MAX)?;
        let plugin_root = fixture.directory.path().join(".sigil/plugins/review");
        let manifest = plugin_root.join("plugin.toml");
        let original = fs::read_to_string(&manifest)?;
        fs::write(
            &manifest,
            format!(
                "{original}\n[[mcp_servers]]\nname = \"echo\"\ntransport = \"stdio\"\ncommand = \"python3\"\nargs = [\"server.py\"]\nstartup = \"lazy\"\nstartup_timeout_secs = 5\n"
            ),
        )?;
        fs::write(
            plugin_root.join("server.py"),
            r#"import json, pathlib, sys
pathlib.Path("launches.txt").write_text("launched")
for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion":"2025-06-18", "serverInfo":{"name":"reviewed","version":"1.0.0"}, "capabilities":{"tools":{}}}
    elif method == "tools/list":
        result = {"tools":[{"name":"echo", "inputSchema":{"type":"object"}, "annotations":{"readOnlyHint":True}}]}
    elif method == "tools/call":
        with pathlib.Path("calls.txt").open("a") as calls:
            calls.write("called\n")
        result = {"content":[{"type":"text","text":"actual-plugin-mcp-call"}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc":"2.0","id":message["id"],"result":result}), flush=True)
"#,
        )?;
        let discovered = discover_workspace_plugins(fixture.directory.path(), &[])?;
        *fixture.source.entries.lock().expect("trust fixture lock") =
            vec![PluginTrustEntry::for_snapshot(
                &discovered.manifests[0],
                PluginTrustDecision::Trusted,
                43,
            )?];
        let config: sigil_kernel::RootConfig = toml::from_str(
            "config_version = 2\n[agent]\nconnection = \"fixture\"\nmodel = \"fixture\"\n",
        )?;
        let capabilities =
            crate::provider_capabilities_for_name("deepseek").expect("provider capability fixture");
        let source: Arc<dyn McpPluginTrustSource> = fixture.source.clone();
        let mut registry = ToolRegistry::new();
        let plugin_servers = crate::register_session_plugin_mcp_tools(
            &mut registry,
            &config,
            &capabilities,
            fixture.directory.path().to_path_buf(),
            source,
            sigil_mcp::unsupported_mcp_elicitation_handler(),
            sigil_mcp::unsupported_mcp_runtime_event_handler(),
            fixture.composition.extension_execution.clone(),
            Arc::new(WorkflowMcpDisclosurePresenter),
            None,
        )
        .await?;
        assert_eq!(plugin_servers.len(), 1);
        assert_eq!(plugin_servers[0].name, "review.echo");
        assert!(!plugin_root.join("launches.txt").exists());
        assert!(registry.spec_for("mcp_catalog").is_some());
        let state = tempfile::tempdir()?;
        let store = sigil_kernel::JsonlSessionStore::new(state.path().join("session.jsonl"))?;
        let context = ToolContext::new(fixture.directory.path(), 5)
            .with_mutation_recorder(sigil_kernel::MutationEventRecorder::new(store.clone()));
        let activation = ToolCall {
            id: "select-reviewed-extension".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name":"review.echo"}).to_string(),
        };
        let approval = registry.permission_plan(&context, &activation)?.subjects;
        let observation = async {
            let activated = registry
                .execute(context.clone().with_approved_subjects(approval), activation)
                .await?;
            anyhow::ensure!(!activated.is_error(), "{}", activated.content);
            let owner = registry
                .lifecycle_owners_by_namespace(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE)
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("activated MCP must expose an exact owner"))?;
            let name = registry.tool_names_by_lifecycle_owner(&owner)[0].clone();
            let call = ToolCall {
                id: "reviewed-echo".to_owned(),
                name,
                args_json: "{}".to_owned(),
            };
            let subjects = registry.permission_plan(&context, &call)?.subjects;
            let authorized = context.clone().with_approved_subjects(subjects);
            let first = execute_reviewed_mcp_with_started_audit(
                &registry,
                &store,
                authorized.clone(),
                call.clone(),
            )
            .await?;
            anyhow::ensure!(!first.is_error(), "{}", first.content);
            anyhow::ensure!(first.content == "actual-plugin-mcp-call");
            // Keep the previously prepared approval and registry. Neither a UI rebind nor a new
            // permission query can be responsible for rejecting this stale physical request.
            if change_manifest {
                let raw = fs::read_to_string(&manifest)?;
                fs::write(
                    &manifest,
                    raw.replace("version = \"1.0.0\"", "version = \"2.0.0\""),
                )?;
            } else {
                fixture
                    .source
                    .entries
                    .lock()
                    .expect("trust fixture lock")
                    .clear();
            }
            let stale = execute_reviewed_mcp_with_started_audit(
                &registry,
                &store,
                authorized,
                ToolCall {
                    id: "reviewed-echo-after-change".to_owned(),
                    ..call
                },
            )
            .await?;
            Ok::<_, anyhow::Error>((
                owner,
                stale,
                fs::read_to_string(plugin_root.join("calls.txt"))?,
            ))
        }
        .await;
        // Explicit cleanup is awaited even if the exercised path returns an error.
        let cleanup = crate::shutdown_mcp_generations(&mut registry).await;
        let (owner, stale, actual_calls) = observation?;
        cleanup?;
        assert!(
            stale.is_error(),
            "old approved call must fail after current trust/declaration changes"
        );
        assert_eq!(
            actual_calls, "called\n",
            "no second tools/call may reach the server"
        );
        let records = store.read_event_records_coordinated()?;
        let lifetime = records
            .iter()
            .map(|record| record.stored_event())
            .filter(|event| {
                event.event_type == "extension_process_lifecycle_recorded"
                    && event.payload["safe_metadata"]["process_generation"] == owner.generation()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lifetime
                .iter()
                .map(|event| event.payload["status"].as_str().expect("status"))
                .collect::<Vec<_>>(),
            ["starting", "running", "stopped"]
        );
        assert!(
            registry
                .lifecycle_owners_by_namespace(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE)
                .is_empty(),
            "completed MCP cleanup must retire its exact lifecycle owner"
        );
    }
    Ok(())
}

struct PendingMcpStartupFixture {
    workflow: WorkflowFixture,
    _state: tempfile::TempDir,
    store: sigil_kernel::JsonlSessionStore,
    config: sigil_kernel::RootConfig,
    registry: ToolRegistry,
    plugin_root: PathBuf,
}

impl PendingMcpStartupFixture {
    async fn new(servers: &[&str]) -> Result<Self> {
        let workflow = WorkflowFixture::new(usize::MAX)?;
        let plugin_root = workflow.directory.path().join(".sigil/plugins/review");
        let manifest = plugin_root.join("plugin.toml");
        let mut raw = fs::read_to_string(&manifest)?;
        for server in servers {
            raw.push_str(&format!("\n[[mcp_servers]]\nname = {server:?}\ntransport = \"stdio\"\ncommand = \"python3\"\nargs = [\"pending.py\", {server:?}]\nstartup = \"eager\"\nstartup_timeout_secs = 30\n"));
        }
        fs::write(&manifest, raw)?;
        fs::write(
            plugin_root.join("pending.py"),
            r#"import json, pathlib, sys, time
name = sys.argv[1]
with pathlib.Path(name + "-launches").open("a") as marker:
    marker.write("launched\n")
for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    if method == "initialize":
        while not pathlib.Path(name + "-release").exists():
            time.sleep(0.01)
        result = {"protocolVersion":"2025-06-18", "serverInfo":{"name":name,"version":"1.0.0"}, "capabilities":{"tools":{}}}
    elif method == "tools/list":
        result = {"tools":[{"name":"echo", "inputSchema":{"type":"object"}}]}
    else:
        result = {"content":[{"type":"text","text":"actual-owned-startup"}]}
    print(json.dumps({"jsonrpc":"2.0","id":message["id"],"result":result}), flush=True)
"#,
        )?;
        let discovered = discover_workspace_plugins(workflow.directory.path(), &[])?;
        *workflow.source.entries.lock().expect("trust fixture lock") =
            vec![PluginTrustEntry::for_snapshot(
                &discovered.manifests[0],
                PluginTrustDecision::Trusted,
                43,
            )?];
        let config: sigil_kernel::RootConfig = toml::from_str(
            "config_version = 2\n[agent]\nconnection = \"fixture\"\nmodel = \"fixture\"\n",
        )?;
        let capabilities = crate::provider_capabilities_for_name("deepseek").expect("capabilities");
        let state = tempfile::tempdir()?;
        let store = sigil_kernel::JsonlSessionStore::new(state.path().join("session.jsonl"))?;
        let context = (
            sigil_kernel::MutationEventRecorder::new(store.clone()),
            sigil_kernel::ExtensionProcessNetworkAdmission::default(),
        );
        let mut registry = ToolRegistry::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            crate::register_session_plugin_mcp_tools(
                &mut registry,
                &config,
                &capabilities,
                workflow.directory.path().to_path_buf(),
                workflow.source.clone(),
                sigil_mcp::unsupported_mcp_elicitation_handler(),
                sigil_mcp::unsupported_mcp_runtime_event_handler(),
                workflow.composition.extension_execution.clone(),
                Arc::new(WorkflowMcpDisclosurePresenter),
                Some(context),
            ),
        )
        .await??;
        // Returning the normal surface must not depend on either initialize gate being released.
        assert!(registry.spec_for("mcp_catalog").is_some());
        for server in servers {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !plugin_root.join(format!("{server}-launches")).exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await?;
        }
        Ok(Self {
            workflow,
            _state: state,
            store,
            config,
            registry,
            plugin_root,
        })
    }

    fn context(&self) -> ToolContext {
        ToolContext::new(self.workflow.directory.path(), 5)
            .with_mutation_recorder(sigil_kernel::MutationEventRecorder::new(self.store.clone()))
    }

    fn call(&self, name: &str) -> Result<(ToolContext, ToolCall)> {
        let call = ToolCall {
            id: format!("explicit-{name}"),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name":format!("review.{name}")}).to_string(),
        };
        let context = self.context();
        let subjects = self.registry.permission_plan(&context, &call)?.subjects;
        Ok((context.with_approved_subjects(subjects), call))
    }
}

#[cfg(unix)]
#[tokio::test]
async fn mcp_prewarm_quiescence_cancels_idle_startup_and_keeps_activation_repairable() -> Result<()>
{
    let mut fixture = PendingMcpStartupFixture::new(&["slow"]).await?;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture
            .registry
            .quiesce_background_work(&sigil_kernel::ToolBackgroundWorkSettlement::CancelIdle),
    )
    .await??;
    assert!(
        fixture
            .registry
            .lifecycle_owners_by_namespace(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE)
            .is_empty(),
        "cancelled prewarm must not publish late tools"
    );
    assert!(fixture.registry.spec_for("mcp_activate_server").is_some());
    let records = fixture.store.read_event_records_coordinated()?;
    let statuses = records
        .iter()
        .map(|record| record.stored_event())
        .filter(|event| {
            event.event_type == "extension_process_lifecycle_recorded"
                && event.payload["safe_metadata"]
                    .get("process_generation")
                    .is_some()
        })
        .map(|event| event.payload["status"].as_str().expect("status"))
        .collect::<Vec<_>>();
    assert_eq!(statuses, ["starting", "running", "stopped"]);
    fs::write(fixture.plugin_root.join("slow-release"), "go")?;
    let (context, call) = fixture.call("slow")?;
    let result = fixture.registry.execute(context, call).await;
    let cleanup = crate::shutdown_mcp_generations(&mut fixture.registry).await;
    assert!(!result?.is_error());
    cleanup?;
    assert_eq!(
        fs::read_to_string(fixture.plugin_root.join("slow-launches"))?,
        "launched\nlaunched\n"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn mcp_parent_quiescence_joins_but_does_not_cancel_explicit_child_activation() -> Result<()> {
    let mut fixture = PendingMcpStartupFixture::new(&["slow"]).await?;
    let (context, call) = fixture.call("slow")?;
    let child_registry = fixture.registry.clone();
    let activation = child_registry.execute(context, call);
    tokio::pin!(activation);
    // Registry dispatch reaches the already-existing in-flight startup on its first poll; the
    // explicit caller lease is installed before that owned JoinHandle is awaited.
    assert!(futures::poll!(activation.as_mut()).is_pending());
    let parent_registry = fixture.registry.clone();
    let mode = sigil_kernel::ToolBackgroundWorkSettlement::CancelIdle;
    let settlement = parent_registry.quiesce_background_work(&mode);
    tokio::pin!(settlement);
    assert!(futures::poll!(settlement.as_mut()).is_pending());
    fs::write(fixture.plugin_root.join("slow-release"), "go")?;
    let (result, settled) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(activation, settlement)
    })
    .await?;
    let cleanup = crate::shutdown_mcp_generations(&mut fixture.registry).await;
    assert!(
        !result?.is_error(),
        "parent preparation must not cancel the admitted child caller"
    );
    settled?;
    cleanup?;
    assert_eq!(
        fs::read_to_string(fixture.plugin_root.join("slow-launches"))?,
        "launched\n",
        "explicit call reuses the prewarm generation"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn mcp_product_activation_joins_only_the_selected_prewarm() -> Result<()> {
    let mut fixture = PendingMcpStartupFixture::new(&["selected", "unrelated"]).await?;
    fs::write(fixture.plugin_root.join("selected-release"), "go")?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::activate_mcp_tools_from_product_surface_with_managed_extension_execution(
            &mut fixture.registry,
            &fixture.config,
            &crate::provider_capabilities_for_name("deepseek").expect("capabilities"),
            fixture.workflow.directory.path().to_path_buf(),
            Some("review.selected"),
            sigil_mcp::unsupported_mcp_elicitation_handler(),
            sigil_mcp::unsupported_mcp_runtime_event_handler(),
            Some(sigil_kernel::MutationEventRecorder::new(
                fixture.store.clone(),
            )),
            sigil_kernel::ExtensionProcessNetworkAdmission::default(),
            None,
            Arc::new(WorkflowMcpDisclosurePresenter),
            fixture.workflow.composition.extension_execution.clone(),
            Some(fixture.workflow.source.clone()),
        ),
    )
    .await;
    let cleanup = crate::shutdown_mcp_generations(&mut fixture.registry).await;
    assert_eq!(result??.matched_servers, 1);
    cleanup?;
    assert!(!fixture.plugin_root.join("unrelated-release").exists());
    assert_eq!(
        fs::read_to_string(fixture.plugin_root.join("selected-launches"))?,
        "launched\n"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_disable_joins_unpublished_prewarm_and_explicit_initialization() -> Result<()> {
    for explicit in [false, true] {
        let mut fixture = PendingMcpStartupFixture::new(&["slow"]).await?;
        let (context, call) = fixture.call("slow")?;
        let registry = fixture.registry.clone();
        let mut activation = Box::pin(registry.execute(context, call));
        if explicit {
            assert!(futures::poll!(activation.as_mut()).is_pending());
        }
        assert!(
            fixture
                .registry
                .lifecycle_owners_by_namespace(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE)
                .is_empty(),
            "the real process has launched but initialize has not published its tools"
        );
        let retirement = crate::prepare_plugin_mcp_retirement(&fixture.registry, "review");
        {
            let mut entries = fixture
                .workflow
                .source
                .entries
                .lock()
                .expect("trust fixture");
            let mut disabled = entries[0].clone();
            disabled.decision = PluginTrustDecision::Disabled;
            entries.push(disabled);
        }
        let (settled, active_result) = if explicit {
            // The first activation poll holds the owned startup join slot while awaiting the
            // real child. Keep driving that caller alongside retirement, as the two actual
            // application requests are driven independently; pausing it would hold the slot
            // forever even after the child stops.
            match tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::join!(retirement.settle(), activation.as_mut())
            })
            .await
            {
                Ok((settled, active)) => (Ok(settled), Some(active)),
                Err(error) => (Err(error), None),
            }
        } else {
            (
                tokio::time::timeout(std::time::Duration::from_secs(5), retirement.settle()).await,
                None,
            )
        };
        // On a failed watchdog, release the actual caller future's join guard before the
        // fallback shutdown joins all remaining owners. The timeout itself is still a failure.
        drop(activation);
        let observed = fixture.store.read_event_records_coordinated();
        let cleanup = crate::shutdown_mcp_generations(&mut fixture.registry).await;
        settled??;
        cleanup?;
        if let Some(result) = active_result {
            assert!(result.is_err() || result.as_ref().is_ok_and(ToolResult::is_error));
        }
        assert!(
            fixture
                .registry
                .lifecycle_owners_by_namespace(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE)
                .is_empty()
        );
        assert_eq!(
            fs::read_to_string(fixture.plugin_root.join("slow-launches"))?,
            "launched\n",
            "revoked explicit activation cannot retry into another physical process"
        );
        let records = observed?;
        assert_cancelled_initialization_is_settled(&records);
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_pending_retirement_catches_publication_and_allows_later_reactivation() -> Result<()>
{
    let mut fixture = PendingMcpStartupFixture::new(&["slow"]).await?;
    let (context, call) = fixture.call("slow")?;
    let registry = fixture.registry.clone();
    let activation = registry.execute(context, call);
    tokio::pin!(activation);
    assert!(futures::poll!(activation.as_mut()).is_pending());
    let retirement = crate::prepare_plugin_mcp_retirement(&fixture.registry, "review");
    fs::write(fixture.plugin_root.join("slow-release"), "go")?;
    assert!(!activation.await?.is_error());
    assert_eq!(
        fixture
            .registry
            .lifecycle_owners_by_namespace(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE)
            .len(),
        1
    );
    retirement.settle().await?;
    assert!(
        fixture
            .registry
            .lifecycle_owners_by_namespace(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE)
            .is_empty(),
        "startup publication after capture remains owned by that exact retirement"
    );
    let (context, call) = fixture.call("slow")?;
    let second = fixture.registry.execute(context, call).await;
    assert!(!second?.is_error());
    let current = fixture
        .registry
        .lifecycle_owners_by_namespace(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE);
    assert_eq!(current.len(), 1);
    let cleanup = crate::shutdown_mcp_generations(&mut fixture.registry).await;
    cleanup?;
    assert_eq!(
        fs::read_to_string(fixture.plugin_root.join("slow-launches"))?,
        "launched\nlaunched\n"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_pending_retirement_fences_capture_to_append_and_releases_failed_review()
-> Result<()> {
    for commit_disable in [false, true] {
        let mut fixture = PendingMcpStartupFixture::new(&["slow"]).await?;
        let unrelated_retirement =
            crate::prepare_plugin_mcp_retirement(&fixture.registry, "unrelated-plugin");
        fixture
            .registry
            .quiesce_background_work(&sigil_kernel::ToolBackgroundWorkSettlement::CancelIdle)
            .await?;
        let retirement = crate::prepare_plugin_mcp_retirement(&fixture.registry, "review");
        let (context, call) = fixture.call("slow")?;
        let blocked = fixture.registry.execute(context, call).await;
        assert!(blocked.is_err() || blocked.as_ref().is_ok_and(ToolResult::is_error));
        assert_eq!(
            fs::read_to_string(fixture.plugin_root.join("slow-launches"))?,
            "launched\n",
            "a new startup must not cross capture to durable decision"
        );
        let (next_context, next_call) = fixture.call("slow")?;
        if commit_disable {
            let mut disabled = fixture
                .workflow
                .source
                .entries
                .lock()
                .expect("trust fixture")[0]
                .clone();
            disabled.decision = PluginTrustDecision::Disabled;
            fixture
                .workflow
                .source
                .entries
                .lock()
                .expect("trust fixture")
                .push(disabled);
            retirement.settle().await?;
        } else {
            drop(retirement); // The manifest/CAS preflight failed: no durable trust change.
        }
        fs::write(fixture.plugin_root.join("slow-release"), "go")?;
        let next = fixture.registry.execute(next_context, next_call).await;
        let cleanup = crate::shutdown_mcp_generations(&mut fixture.registry).await;
        cleanup?;
        drop(unrelated_retirement);
        if commit_disable {
            assert!(next.is_err() || next.as_ref().is_ok_and(ToolResult::is_error));
            assert_eq!(
                fs::read_to_string(fixture.plugin_root.join("slow-launches"))?,
                "launched\n"
            );
        } else {
            assert!(
                !next?.is_error(),
                "dropping an uncommitted review cannot leave a permanent gate"
            );
            assert_eq!(
                fs::read_to_string(fixture.plugin_root.join("slow-launches"))?,
                "launched\nlaunched\n"
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_review_publication_error_still_joins_actual_initializing_process() -> Result<()> {
    for ambiguous_error in [false, true] {
        let mut fixture = PendingMcpStartupFixture::new(&["slow"]).await?;
        let retirement = crate::prepare_plugin_mcp_retirement(&fixture.registry, "review");
        let mut session =
            sigil_kernel::Session::new("fixture", "fixture").with_store(fixture.store.clone());
        let scope = session.session_scope_id().to_owned();
        let declaration = discover_workspace_plugins(fixture.workflow.directory.path(), &[])?
            .manifests
            .remove(0);
        let request = crate::plugin_management::ApplicationPluginDecisionRequest {
            plugin_id: declaration.plugin_id.clone(),
            expected_manifest_hash: declaration.manifest_hash.clone(),
            expected_capability_digest: declaration.capability_digest()?,
            decision: PluginTrustDecision::Disabled,
        };
        let publication = crate::plugin_management::apply_application_plugin_decision_to_session(
            &mut session,
            &scope,
            fixture.workflow.directory.path(),
            &request,
        );
        publication
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))?;
        {
            let mut trust = fixture
                .workflow
                .source
                .entries
                .lock()
                .expect("trust fixture");
            let mut disabled = trust[0].clone();
            disabled.decision = PluginTrustDecision::Disabled;
            trust.push(disabled);
        }
        // The durable append happened but its caller received an I/O ambiguity. This is the
        // production settlement seam, not a test-only branch or a dropped-resource sentinel.
        let publication = if ambiguous_error {
            Err(
                crate::application_operation_owner::ApplicationPublicationError(anyhow::anyhow!(
                    "post-publication fixture I/O error"
                ))
                .into(),
            )
        } else {
            publication
        };
        let result = crate::plugin_management::settle_application_plugin_decision(
            &mut session,
            publication,
            vec![retirement],
        )
        .await;
        // Capture the tested seam's state before fixture cleanup can repair a missed join.
        let observed = fixture.store.read_event_records_coordinated();
        let cleanup = crate::shutdown_mcp_generations(&mut fixture.registry).await;
        cleanup?;
        if ambiguous_error {
            assert!(result.is_err());
        } else {
            assert_eq!(
                result?.0.process_cleanup,
                Some(sigil_kernel::PluginCleanupStatus::Confirmed)
            );
        }
        let records = observed?;
        assert_cancelled_initialization_is_settled(&records);
        let completed = records
            .iter()
            .filter_map(|record| record.session_log_entry().ok().flatten())
            .filter(|entry| {
                matches!(
                    entry,
                    sigil_kernel::SessionLogEntry::Control(
                        sigil_kernel::ControlEntry::PluginReviewCompletedV1(_)
                    )
                )
            })
            .count();
        assert_eq!(completed, usize::from(!ambiguous_error));
    }
    Ok(())
}

#[cfg(unix)]
fn assert_cancelled_initialization_is_settled(records: &[sigil_kernel::SessionStreamRecord]) {
    let events = records
        .iter()
        .map(|record| record.stored_event())
        .filter(|event| event.event_type == "extension_process_lifecycle_recorded")
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 4);
    let generation = events[0].payload["safe_metadata"]["process_generation"]
        .as_str()
        .expect("exact generation");
    assert!(!generation.is_empty());
    for (event, status) in events[..3].iter().zip(["starting", "running", "stopped"]) {
        assert_eq!(event.payload["status"], status);
        assert_eq!(
            event.payload["safe_metadata"]["process_generation"],
            generation
        );
        assert_eq!(event.payload["subject"], events[0].payload["subject"]);
    }
    // The older startup-result audit follows real cleanup; it is not another generation state.
    assert_eq!(events[3].payload["status"], "startup_failed");
    assert!(
        events[3].payload["safe_metadata"]
            .get("process_generation")
            .is_none()
    );
    assert_eq!(events[3].payload["subject"], events[0].payload["subject"]);
}

#[tokio::test]
async fn application_mcp_explicit_environment_reaches_managed_child_and_binds_request_approval()
-> Result<()> {
    let fixture = WorkflowFixture::new(usize::MAX)?;
    let workspace = fixture.directory.path();
    let environment_name = format!("SIGIL_C2_CLIENT_{}", uuid::Uuid::new_v4().simple());
    // This client-only value never enters the parent environment. A request-time fallback to
    // inherited environment grants therefore cannot accidentally validate the running child.
    assert!(std::env::var_os(&environment_name).is_none());
    let values = ["client-private-value-first", "client-private-value-second"];
    fs::write(
        workspace.join("explicit-environment.py"),
        r#"import hashlib, json, os, pathlib, sys
with pathlib.Path("explicit-launches.txt").open("a") as marker:
    marker.write("launched\n")
for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion":"2025-06-18", "serverInfo":{"name":"client-env","version":"1.0.0"}, "capabilities":{"tools":{}}}
    elif method == "tools/list":
        result = {"tools":[{"name":"fingerprint", "inputSchema":{"type":"object"}, "annotations":{"readOnlyHint":True}}]}
    elif method == "tools/call":
        with pathlib.Path("explicit-calls.txt").open("a") as marker:
            marker.write("called\n")
        value = os.environ.get(sys.argv[1], "absent").encode()
        result = {"content":[{"type":"text","text":hashlib.sha256(value).hexdigest()}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc":"2.0","id":message["id"],"result":result}), flush=True)
"#,
    )?;
    let state = tempfile::tempdir()?;
    let store = sigil_kernel::JsonlSessionStore::new(state.path().join("session.jsonl"))?;
    let context = ToolContext::new(workspace, 5)
        .with_mutation_recorder(sigil_kernel::MutationEventRecorder::new(store.clone()));
    let capabilities =
        crate::provider_capabilities_for_name("deepseek").expect("provider capability fixture");
    let mut registries = [ToolRegistry::new(), ToolRegistry::new()];
    for (registry, value) in registries.iter_mut().zip(values) {
        let declaration = crate::ApplicationMcpServerDeclaration {
            server: sigil_kernel::McpServerConfig {
                name: "client-env".to_owned(),
                startup: sigil_kernel::McpServerStartup::Lazy,
                startup_timeout_secs: 5,
                transport: sigil_kernel::McpServerTransportConfig::Stdio {
                    command: "python3".to_owned(),
                    args: vec![
                        "explicit-environment.py".to_owned(),
                        environment_name.clone(),
                    ],
                    inherit_env: Vec::new(),
                },
                ..Default::default()
            },
            environment: std::collections::BTreeMap::from([(
                environment_name.clone(),
                sigil_kernel::SecretString::new(value),
            )]),
        };
        let mut config: sigil_kernel::RootConfig = toml::from_str(
            "config_version = 2\n[agent]\nconnection = \"fixture\"\nmodel = \"fixture\"\n",
        )?;
        let environments =
            crate::application_mcp::environments(std::slice::from_ref(&declaration))?;
        crate::application_mcp::merge(&mut config, std::slice::from_ref(&declaration))?;
        assert!(!toml::to_string(&config)?.contains(value));
        crate::mcp_registry::register_session_plugin_mcp_tools_with_registry_slot(
            registry,
            &config,
            &capabilities,
            workspace.to_path_buf(),
            fixture.source.clone(),
            sigil_mcp::unsupported_mcp_elicitation_handler(),
            sigil_mcp::unsupported_mcp_runtime_event_handler(),
            fixture.composition.extension_execution.clone(),
            Arc::new(WorkflowMcpDisclosurePresenter),
            None,
            None,
            environments,
        )
        .await?;
    }
    assert!(!workspace.join("explicit-launches.txt").exists());
    let activation = ToolCall {
        id: "client-env-activation".to_owned(),
        name: "mcp_activate_server".to_owned(),
        args_json: json!({"server_name":"client-env"}).to_string(),
    };
    let first_subjects = registries[0]
        .permission_plan(&context, &activation)?
        .subjects;
    let second_subjects = registries[1]
        .permission_plan(&context, &activation)?
        .subjects;
    assert_ne!(first_subjects, second_subjects);
    let observation = async {
        let mut owners = Vec::new();
        for (index, registry) in registries.iter().enumerate() {
            if index == 1 {
                // Same declaration/scope/name, different explicit values: the first process's
                // approval must not authorize a replacement process with the second value.
                let rejected = registry
                    .execute(
                        context
                            .clone()
                            .with_approved_subjects(first_subjects.clone()),
                        activation.clone(),
                    )
                    .await;
                anyhow::ensure!(rejected.is_err(), "changed values reused stale approval");
                anyhow::ensure!(
                    fs::read_to_string(workspace.join("explicit-launches.txt"))? == "launched\n",
                    "stale approval physically started a second child"
                );
                let error = format!("{:#}", rejected.expect_err("checked rejected result"));
                anyhow::ensure!(values.iter().all(|value| !error.contains(value)));
            }
            let subjects = if index == 0 {
                first_subjects.clone()
            } else {
                second_subjects.clone()
            };
            let activated = registry
                .execute(
                    context.clone().with_approved_subjects(subjects),
                    activation.clone(),
                )
                .await?;
            anyhow::ensure!(!activated.is_error(), "{}", activated.content);
            let owner = registry
                .lifecycle_owners_by_namespace(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE)
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("managed child must expose its exact generation"))?;
            let name = registry
                .tool_names_by_lifecycle_owner(&owner)
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("managed child did not publish its tool"))?;
            let call = ToolCall {
                id: format!("client-env-call-{index}"),
                name,
                args_json: "{}".to_owned(),
            };
            let subjects = registry.permission_plan(&context, &call)?.subjects;
            let authorized = context.clone().with_approved_subjects(subjects);
            // The production agent persists this profile before dispatch. Missing that record
            // must still reject the call before the real child receives any tools/call request.
            let unaudited = registry
                .execute(authorized.clone(), call.clone())
                .await
                .expect_err("local MCP calls require the persisted execution profile");
            anyhow::ensure!(
                unaudited
                    .to_string()
                    .contains("requires persisted ToolExecutionStarted"),
                "unexpected unaudited rejection: {unaudited:#}"
            );
            let calls_before =
                fs::read_to_string(workspace.join("explicit-calls.txt")).or_else(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        Ok(String::new())
                    } else {
                        Err(error)
                    }
                })?;
            anyhow::ensure!(calls_before == "called\n".repeat(index * 2));
            // Each physical request reruns the client's environment-source check. Match the
            // existing agent boundary: append Started with its actual profile, then dispatch.
            for request_index in 0..2 {
                let call = ToolCall {
                    id: format!("client-env-call-{index}-{request_index}"),
                    ..call.clone()
                };
                let mut started = sigil_kernel::durable_tool_execution_entry(
                    &call,
                    authorized.approved_subjects(),
                    sigil_kernel::ToolExecutionStatus::Started,
                    None,
                    None,
                )?;
                let profile = registry
                    .execution_mutation_profile(&authorized, &call)?
                    .ok_or_else(|| anyhow::anyhow!("local MCP execution profile is required"))?;
                started.metadata.details["execution_mutation_profile"] =
                    serde_json::to_value(profile)?;
                store.append(&sigil_kernel::SessionLogEntry::Control(
                    ControlEntry::ToolExecution(Box::new(started)),
                ))?;
                let result = registry
                    .execute_after_started_audit(authorized.clone(), call.clone())
                    .await?;
                let finished = sigil_kernel::durable_tool_execution_entry(
                    &call,
                    authorized.approved_subjects(),
                    if result.is_error() {
                        sigil_kernel::ToolExecutionStatus::Failed
                    } else {
                        sigil_kernel::ToolExecutionStatus::Completed
                    },
                    Some(0),
                    Some(&result),
                )?;
                store.append(&sigil_kernel::SessionLogEntry::Control(
                    ControlEntry::ToolExecution(Box::new(finished)),
                ))?;
                anyhow::ensure!(!result.is_error(), "{}", result.content);
                anyhow::ensure!(
                    result.content == format!("{:x}", Sha256::digest(values[index].as_bytes())),
                    "the actual child did not receive this declaration's explicit value"
                );
            }
            owners.push(owner);
        }
        anyhow::ensure!(owners[0].generation() != owners[1].generation());
        Ok::<_, anyhow::Error>(owners)
    }
    .await;
    // Every constructed owner is settled even if a request or a negative assertion failed.
    let mut cleanup_errors = Vec::new();
    for registry in &mut registries {
        if let Err(error) = crate::shutdown_mcp_generations(registry).await {
            cleanup_errors.push(format!("{error:#}"));
        }
    }
    anyhow::ensure!(cleanup_errors.is_empty(), "{}", cleanup_errors.join("; "));
    let owners = observation?;
    assert_eq!(
        fs::read_to_string(workspace.join("explicit-launches.txt"))?,
        "launched\nlaunched\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("explicit-calls.txt"))?,
        "called\ncalled\ncalled\ncalled\n"
    );
    assert!(std::env::var_os(&environment_name).is_none());
    let records = store.read_event_records_coordinated()?;
    let entries = sigil_kernel::JsonlSessionStore::read_entries(store.path())?;
    let executions = entries
        .iter()
        .filter_map(|entry| match entry {
            sigil_kernel::SessionLogEntry::Control(ControlEntry::ToolExecution(entry)) => {
                Some(entry.as_ref())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(executions.len(), 8);
    for (pair, call_id) in executions.chunks_exact(2).zip([
        "client-env-call-0-0",
        "client-env-call-0-1",
        "client-env-call-1-0",
        "client-env-call-1-1",
    ]) {
        assert_eq!(pair[0].call_id, call_id);
        assert_eq!(pair[0].status, sigil_kernel::ToolExecutionStatus::Started);
        assert!(pair[0].metadata.details["execution_mutation_profile"].is_object());
        assert_eq!(pair[1].call_id, call_id);
        assert_eq!(pair[1].status, sigil_kernel::ToolExecutionStatus::Completed);
    }
    for owner in owners {
        let statuses = records
            .iter()
            .map(|record| record.stored_event())
            .filter(|event| {
                event.event_type == "extension_process_lifecycle_recorded"
                    && event.payload["safe_metadata"]["process_generation"] == owner.generation()
            })
            .map(|event| event.payload["status"].as_str().expect("lifecycle status"))
            .collect::<Vec<_>>();
        assert_eq!(statuses, ["starting", "running", "stopped"]);
    }
    let persisted = format!(
        "{}\n{}",
        fs::read_to_string(store.path())?,
        fixture.records()?
    );
    for value in values {
        assert!(
            !persisted.contains(value),
            "explicit process value leaked into audit"
        );
    }
    Ok(())
}
