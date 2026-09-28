//! Initialization work owned by the existing activation tool; no process authority lives here.

use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use super::*;

#[derive(Default)]
pub(super) struct McpActivationStartups {
    state: Mutex<StartupState>,
}

#[derive(Default)]
struct StartupState {
    pending: BTreeMap<String, Arc<PendingStartup>>,
    retiring_origins: BTreeMap<(String, String), usize>,
    closed: bool,
}

/// A short local transaction fence on the existing startup owner. It neither grants execution
/// nor persists trust; dropping a failed review releases it without changing the declaration.
struct StartupRetirementFence {
    startups: Arc<McpActivationStartups>,
    origin: (String, String),
}

impl Drop for StartupRetirementFence {
    fn drop(&mut self) {
        let mut state = self
            .startups
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = state.retiring_origins.get_mut(&self.origin) {
            *count -= 1;
            if *count == 0 {
                state.retiring_origins.remove(&self.origin);
            }
        }
    }
}

struct PendingStartup {
    cancellation: sigil_kernel::RunCancellationOwner,
    origin: Option<sigil_kernel::ToolLifecycleOrigin>,
    published_owners: Arc<Mutex<Vec<sigil_kernel::ToolLifecycleOwner>>>,
    cleanup: Arc<Mutex<Option<std::result::Result<(), String>>>>,
    explicit_callers: AtomicUsize,
    prewarm: bool,
    joined: tokio::sync::Mutex<StartupJoin>,
}

struct StartupJoin {
    task: Option<tokio::task::JoinHandle<Result<LazyMcpActivationResult>>>,
    outcome: Option<std::result::Result<LazyMcpActivationResult, String>>,
}

struct ExplicitStartupCaller(Arc<PendingStartup>);

impl Drop for ExplicitStartupCaller {
    fn drop(&mut self) {
        self.0.explicit_callers.fetch_sub(1, Ordering::SeqCst);
    }
}

impl PendingStartup {
    async fn join(&self) -> Result<LazyMcpActivationResult> {
        let mut joined = self.joined.lock().await;
        if let Some(task) = joined.task.as_mut() {
            let result = task.await;
            joined.task.take();
            joined.outcome = Some(match result {
                Ok(result) => result.map_err(|error| format!("{error:#}")),
                Err(error) => Err(format!("MCP startup owner join failed: {error}")),
            });
        }
        match &joined.outcome {
            Some(Ok(result)) => Ok(result.clone()),
            Some(Err(error)) => Err(anyhow!(error.clone())),
            None => bail!("MCP startup owner has no outcome"),
        }
    }
}

impl McpActivationStartups {
    fn start(
        &self,
        tool: &McpActivateServerTool,
        server_name: &str,
        context: StartupInputs,
        explicit: bool,
    ) -> Result<(Arc<PendingStartup>, Option<ExplicitStartupCaller>)> {
        let origin = tool
            .plugin_declarations
            .iter()
            .find(|declaration| declaration.effective_name() == server_name)
            .and_then(|declaration| match declaration.origin() {
                McpConfigOrigin::PluginManifest {
                    plugin_id,
                    manifest_hash,
                    ..
                } => Some(sigil_kernel::ToolLifecycleOrigin {
                    namespace: "plugin".to_owned(),
                    subject: plugin_id.clone(),
                    revision: manifest_hash.clone(),
                }),
                _ => None,
            });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anyhow::ensure!(!state.closed, "MCP activation owner has shut down");
        anyhow::ensure!(
            origin.as_ref().is_none_or(|origin| !state
                .retiring_origins
                .contains_key(&(origin.namespace.clone(), origin.subject.clone()))),
            "plugin MCP review is in progress; retry activation after the review completes"
        );
        let pending = if let Some(pending) = state.pending.get(server_name) {
            Arc::clone(pending)
        } else if tool.registered_tool_count(server_name) > 0 {
            // A caller can pass the public readiness check before another caller publishes and
            // removes its completed startup. Recheck under the same lock that owns that removal.
            Arc::new(PendingStartup {
                cancellation: sigil_kernel::RunCancellationOwner::new(),
                origin: None,
                published_owners: Arc::default(),
                cleanup: Arc::new(Mutex::new(Some(Ok(())))),
                explicit_callers: AtomicUsize::new(0),
                prewarm: false,
                joined: tokio::sync::Mutex::new(StartupJoin {
                    task: None,
                    outcome: Some(Ok(LazyMcpActivationResult {
                        matched_servers: 1,
                        added_tools: 0,
                        process_launch_receipts: Vec::new(),
                    })),
                }),
            })
        } else {
            let cancellation = sigil_kernel::RunCancellationOwner::new();
            let handle = cancellation.handle();
            let published_owners = Arc::new(Mutex::new(Vec::new()));
            let cleanup = Arc::new(Mutex::new(None));
            // Copy only launch inputs. In particular, do not capture the activation tool or its
            // registry strongly from a task whose JoinHandle that tool itself owns.
            let launch = StartupLaunch {
                registry: tool.registry.clone(),
                root_config: tool.root_config.clone(),
                capabilities: tool.provider_capabilities.clone(),
                workspace: tool.workspace_root.clone(),
                elicitation: Arc::clone(&tool.elicitation_handler),
                events: Arc::clone(&tool.runtime_event_handler),
                managed: tool.managed_extension_execution.clone(),
                trust: tool.plugin_trust_source.clone(),
                process_environments: Arc::clone(&tool.process_environments),
                server_name: server_name.to_owned(),
                context,
                published_owners: Arc::clone(&published_owners),
                cleanup: Arc::clone(&cleanup),
            };
            let task = tokio::spawn(launch.run(handle));
            let pending = Arc::new(PendingStartup {
                cancellation,
                origin,
                published_owners,
                cleanup,
                explicit_callers: AtomicUsize::new(0),
                prewarm: !explicit,
                joined: tokio::sync::Mutex::new(StartupJoin {
                    task: Some(task),
                    outcome: None,
                }),
            });
            state
                .pending
                .insert(server_name.to_owned(), Arc::clone(&pending));
            pending
        };
        let caller = explicit.then(|| {
            pending.explicit_callers.fetch_add(1, Ordering::SeqCst);
            ExplicitStartupCaller(Arc::clone(&pending))
        });
        Ok((pending, caller))
    }

    pub(super) fn prewarm(
        &self,
        tool: &McpActivateServerTool,
        server_name: &str,
        recorder: MutationEventRecorder,
        network_admission: ExtensionProcessNetworkAdmission,
    ) -> Result<()> {
        self.start(
            tool,
            server_name,
            StartupInputs {
                mutation_recorder: Some(recorder),
                expected_subject: None,
                network_admission,
            },
            false,
        )
        .map(|_| ())
    }

    pub(super) fn prepare_retirement(
        self: &Arc<Self>,
        namespace: &str,
        subject: &str,
        registry: sigil_kernel::WeakToolRegistry,
    ) -> Vec<futures::future::BoxFuture<'static, Result<()>>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let origin = (namespace.to_owned(), subject.to_owned());
        *state.retiring_origins.entry(origin.clone()).or_default() += 1;
        let fence = StartupRetirementFence {
            startups: Arc::clone(self),
            origin,
        };
        let pending = state
            .pending
            .values()
            .filter(|pending| {
                pending.origin.as_ref().is_some_and(|origin| {
                    origin.namespace == namespace && origin.subject == subject
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        drop(state);
        vec![Box::pin(async move {
            let _fence = fence;
            let mut all_failures = Vec::new();
            for pending in pending {
                pending.cancellation.request_cancel();
                let outcome = pending.join().await;
                let cleanup = pending
                    .cleanup
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let owners = pending
                    .published_owners
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let mut failures = Vec::new();
                match cleanup {
                    Some(Ok(())) => {}
                    Some(Err(error)) => failures.push(error),
                    None => {
                        failures.push(format!("MCP startup did not confirm cleanup: {outcome:?}"))
                    }
                }
                if !owners.is_empty() {
                    if let Some(mut registry) = registry.upgrade() {
                        let retirements = owners
                            .iter()
                            .map(|owner| registry.retire_by_lifecycle_owner(owner))
                            .collect::<Vec<_>>();
                        for retirement in retirements {
                            if let Err(error) = retirement.dispose_and_quiesce().await {
                                failures.push(format!("{error:#}"));
                            }
                        }
                    } else {
                        failures.push(
                            "MCP registry closed before captured publication was retired"
                                .to_owned(),
                        );
                    }
                }
                all_failures.extend(failures);
            }
            anyhow::ensure!(
                all_failures.is_empty(),
                "MCP startup retirement incomplete: {}",
                all_failures.join("; ")
            );
            Ok(())
        })]
    }

    pub(super) async fn activate(
        &self,
        tool: &McpActivateServerTool,
        server_name: &str,
        context: ToolContext,
    ) -> Result<LazyMcpActivationResult> {
        // An eager attempt can have been cancelled by final verification, or refused network
        // admission. A new explicit approval may retry once with its own exact request context.
        for attempt in 0..2 {
            let (pending, caller) = self.start(
                tool,
                server_name,
                StartupInputs::from_context(&context),
                true,
            )?;
            let result = if let Some(cancellation) = context.cancellation_handle() {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        drop(caller);
                        let cancelled_idle = {
                            let _state = self.state.lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            // Claiming an explicit caller and cancelling the last caller's
                            // startup must be atomic with respect to one another.
                            let idle = pending.explicit_callers.load(Ordering::SeqCst) == 0;
                            if idle {
                                pending.cancellation.request_cancel();
                            }
                            idle
                        };
                        if cancelled_idle {
                            let _outcome = pending.join().await;
                        }
                        bail!("MCP activation cancelled");
                    }
                    result = pending.join() => result,
                }
            } else {
                pending.join().await
            };
            drop(caller);
            // Always retire joined bookkeeping. The actual process owner remains in the registry,
            // while a future repair can activate again after a failed attempt or deactivation.
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state
                    .pending
                    .get(server_name)
                    .is_some_and(|current| Arc::ptr_eq(current, &pending))
                {
                    state.pending.remove(server_name);
                }
            }
            if result.is_err()
                && (pending.prewarm || pending.cancellation.handle().is_cancel_requested())
                && attempt == 0
            {
                continue;
            }
            return result;
        }
        bail!("MCP activation could not complete")
    }

    pub(super) async fn join_scope(&self, server_name: &str) -> Result<()> {
        let pending = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.pending.get(server_name).cloned().map(|pending| {
                pending.explicit_callers.fetch_add(1, Ordering::SeqCst);
                (Arc::clone(&pending), ExplicitStartupCaller(pending))
            })
        };
        if let Some((pending, _caller)) = pending
            && pending.join().await.is_err()
        {
            tracing::debug!(
                "selected MCP prewarm did not become ready; explicit activation may retry"
            );
        }
        Ok(())
    }

    pub(super) async fn quiesce(&self, shutdown: bool) -> Result<()> {
        let pending = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed |= shutdown;
            state
                .pending
                .values()
                .cloned()
                .inspect(|pending| {
                    // Parent verification must not cancel an activation already claimed by an
                    // explicit caller, including a live background child.
                    if shutdown || pending.explicit_callers.load(Ordering::SeqCst) == 0 {
                        pending.cancellation.request_cancel();
                    }
                })
                .collect::<Vec<_>>()
        };
        for pending in pending {
            if pending.join().await.is_err() {
                // Initialization failure is not a verification gate. Process quiescence and
                // unresolved mutation evidence remain the authority's durable decision.
                tracing::debug!("owned MCP initialization did not become ready");
            }
        }
        Ok(())
    }
}

struct StartupInputs {
    mutation_recorder: Option<MutationEventRecorder>,
    expected_subject: Option<ToolSubject>,
    network_admission: ExtensionProcessNetworkAdmission,
}

impl StartupInputs {
    fn from_context(context: &ToolContext) -> Self {
        Self {
            mutation_recorder: context.mutation_recorder.clone(),
            expected_subject: context
                .approved_subjects()
                .iter()
                .find(|subject| subject.kind == ToolSubjectKind::McpTrustClass)
                .cloned(),
            network_admission: ExtensionProcessNetworkAdmission::new(
                context.network_policy(),
                context.explicit_network_approval(),
            ),
        }
    }
}

struct StartupLaunch {
    registry: sigil_kernel::WeakToolRegistry,
    root_config: RootConfig,
    capabilities: ProviderCapabilities,
    workspace: PathBuf,
    elicitation: Arc<dyn McpElicitationHandler>,
    events: Arc<dyn McpRuntimeEventHandler>,
    managed: Option<Arc<crate::managed_resource_adapters::RuntimeManagedExtensionExecutionRouteV1>>,
    trust: Option<Arc<dyn McpPluginTrustSource>>,
    process_environments: crate::application_mcp::ProcessEnvironments,
    server_name: String,
    context: StartupInputs,
    published_owners: Arc<Mutex<Vec<sigil_kernel::ToolLifecycleOwner>>>,
    cleanup: Arc<Mutex<Option<std::result::Result<(), String>>>>,
}

impl StartupLaunch {
    async fn run(
        self,
        cancellation: sigil_kernel::RunCancellationHandle,
    ) -> Result<LazyMcpActivationResult> {
        let mut staging = ToolRegistry::new();
        let result = activate_lazy_mcp_tools_detailed_inner(
            &mut staging,
            &self.root_config,
            &self.capabilities,
            self.workspace.clone(),
            Some(&self.server_name),
            self.elicitation,
            self.events,
            self.context.mutation_recorder.clone(),
            self.context.expected_subject.clone(),
            self.managed.clone(),
            self.context.network_admission,
            self.trust.clone(),
            Some(cancellation.clone()),
            Arc::clone(&self.process_environments),
        )
        .await;
        let publish = async {
            let result = result?;
            anyhow::ensure!(
                result.matched_servers > 0,
                "MCP declaration is no longer available"
            );
            anyhow::ensure!(
                !cancellation.is_cancel_requested(),
                "MCP initialization cancelled before publication"
            );
            let declarations = resolve_runtime_mcp_declarations(
                &self.root_config,
                &self.workspace,
                self.trust.clone(),
            )
            .await?;
            let selected = declarations
                .into_iter()
                .filter(|declaration| declaration.effective_name() == self.server_name)
                .collect::<Vec<_>>();
            anyhow::ensure!(
                selected.len() == 1,
                "MCP declaration changed before publication"
            );
            let launcher = declaration_mcp_process_launcher(
                &self.root_config,
                &selected,
                self.trust.clone(),
                self.managed.clone(),
                Arc::clone(&self.process_environments),
            )?;
            let current = launcher.resolve_launch_request(selected[0].config(), None)?;
            anyhow::ensure!(
                result
                    .process_launch_receipts
                    .iter()
                    .all(|receipt| receipt.declaration == current.declaration
                        && receipt.launch_static_fingerprint == current.launch_static_fingerprint
                        && receipt.environment_live_fingerprint
                            == current.environment.live_fingerprint()),
                "MCP process binding changed before publication"
            );
            let _publish_effect = cancellation.begin_effect(
                sigil_kernel::RunEffectClass::Forward,
                sigil_kernel::RunEffectKind::Tool,
            )?;
            let mut registry = self
                .registry
                .upgrade()
                .ok_or_else(|| anyhow!("MCP registry attachment closed before publication"))?;
            let owners =
                staging.lifecycle_owners_by_namespace(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE);
            let incoming = owners
                .iter()
                .flat_map(|owner| staging.retire_by_lifecycle_owner(owner).tools())
                .collect::<Vec<_>>();
            let publication = registry.register_batch_if_vacant(&incoming);
            if let Err(error) = publication {
                if let Err(cleanup) = shutdown_registered_tools(&incoming).await {
                    *self
                        .cleanup
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(Err(format!("{cleanup:#}")));
                    return Err(cleanup.context(error));
                }
                return Err(error);
            }
            *self
                .published_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = owners;
            Ok::<_, anyhow::Error>(result)
        }
        .await;
        let cleanup = if publish.is_err() {
            shutdown_mcp_generations(&mut staging).await
        } else {
            Ok(())
        };
        {
            let mut recorded = self
                .cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if recorded.is_none() {
                *recorded = Some(
                    cleanup
                        .as_ref()
                        .map_err(|error| format!("{error:#}"))
                        .copied(),
                );
            }
        }
        cleanup?;
        publish
    }
}
