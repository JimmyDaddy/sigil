//! Evidence attached to the existing MCP client owner, from launch attempt through quiescence.

use super::*;

pub(super) struct McpProcessLifetime {
    recorder: MutationEventRecorder,
    workspace_root: PathBuf,
    final_scan_baseline: std::sync::Mutex<Option<WorkspaceMutationScan>>,
    final_scan_armed: std::sync::atomic::AtomicBool,
    pub(super) owner: ToolLifecycleOwner,
    metadata: BTreeMap<String, String>,
    settled: tokio::sync::Mutex<bool>,
}

impl McpProcessLifetime {
    pub(super) fn new(
        options: &McpToolRegistrationOptions,
        server_name: &str,
    ) -> Option<Arc<Self>> {
        Some(Arc::new(Self {
            recorder: options.mutation_recorder.clone()?,
            workspace_root: options.mutation_workspace_root.clone()?,
            final_scan_baseline: std::sync::Mutex::new(None),
            final_scan_armed: std::sync::atomic::AtomicBool::new(false),
            owner: ToolLifecycleOwner::new(
                MCP_TOOL_LIFECYCLE_NAMESPACE,
                server_name,
                Uuid::new_v4().to_string(),
            ),
            metadata: options
                .pre_spawn_safe_metadata
                .get(server_name)
                .cloned()
                .unwrap_or_default(),
            settled: tokio::sync::Mutex::new(false),
        }))
    }

    pub(super) async fn starting(self: &Arc<Self>) -> Result<()> {
        let this = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            this.append(ExtensionProcessLifecycleStatus::Starting, None)
        })
        .await?
    }

    pub(super) async fn running(self: &Arc<Self>, receipt: &McpProcessLaunchReceipt) -> Result<()> {
        let this = Arc::clone(self);
        let receipt = receipt.clone();
        tokio::task::spawn_blocking(move || {
            this.append(ExtensionProcessLifecycleStatus::Running, Some(&receipt))
        })
        .await?
    }

    pub(super) async fn finish(
        self: &Arc<Self>,
        receipt: Option<&McpProcessLaunchReceipt>,
        cleanup_completed: bool,
    ) -> Result<()> {
        let mut settled = self.settled.lock().await;
        if *settled {
            return Ok(());
        }
        let this = Arc::clone(self);
        let receipt = receipt.cloned();
        tokio::task::spawn_blocking(move || {
            this.finish_blocking(receipt.as_ref(), cleanup_completed)
        })
        .await??;
        *settled = true;
        Ok(())
    }

    pub(super) async fn rejected_before_spawn(self: &Arc<Self>) -> Result<()> {
        let mut settled = self.settled.lock().await;
        if *settled {
            return Ok(());
        }
        let this = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            this.append(ExtensionProcessLifecycleStatus::Stopped, None)
        })
        .await??;
        *settled = true;
        Ok(())
    }

    pub(super) fn arm_final_scan(&self, baseline: Option<WorkspaceMutationScan>) {
        *self
            .final_scan_baseline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = baseline;
        self.final_scan_armed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    fn metadata(&self, receipt: Option<&McpProcessLaunchReceipt>) -> BTreeMap<String, String> {
        let mut metadata = self.metadata.clone();
        if let Some(receipt) = receipt {
            metadata.extend(receipt.audit_metadata());
        }
        metadata.insert(
            "process_generation".to_owned(),
            self.owner.generation().to_owned(),
        );
        metadata.insert("workspace_effect".to_owned(), "unknown".to_owned());
        metadata
    }

    fn append(
        &self,
        status: ExtensionProcessLifecycleStatus,
        receipt: Option<&McpProcessLaunchReceipt>,
    ) -> Result<()> {
        self.recorder
            .append_extension_process_lifecycle(&ExtensionProcessLifecycleAudit {
                process_kind: "mcp_stdio".to_owned(),
                subject: self.owner.scope().to_owned(),
                phase: if receipt.is_some() {
                    ExtensionProcessLaunchPhase::PostSpawn
                } else {
                    ExtensionProcessLaunchPhase::PreSpawn
                },
                status,
                safe_metadata: self.metadata(receipt),
            })?;
        Ok(())
    }

    fn finish_blocking(
        &self,
        receipt: Option<&McpProcessLaunchReceipt>,
        cleanup_completed: bool,
    ) -> Result<()> {
        let metadata = self.metadata(receipt);
        let process_name = format!("mcp_server:{}", self.owner.scope());
        // Startup failure is scanned by registration; an active generation starts a new scan
        // interval only after its startup result has been recorded.
        if self
            .final_scan_armed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            let baseline = self
                .final_scan_baseline
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            // Every supported local profile permits workspace writes. Tool annotations cannot
            // prove confinement, so the final interval must be checked after actual cleanup.
            if cleanup_completed && let Some(before) = &baseline {
                match self
                    .recorder
                    .capture_workspace_scan(&self.workspace_root, &before.scope)
                {
                    Ok(after) => {
                        self.recorder.record_external_process_mutation_scan_result(
                            before,
                            &after,
                            process_name,
                            ToolEffect::Unknown,
                            metadata,
                        )?;
                    }
                    Err(_) => {
                        self.recorder
                            .record_external_process_scan_unavailable_after(
                                before,
                                process_name,
                                ToolEffect::Unknown,
                                metadata,
                            )?;
                    }
                }
            } else {
                self.recorder
                    .record_external_process_unknown_dirty_with_metadata(
                        &self.workspace_root,
                        process_name,
                        ToolEffect::Unknown,
                        metadata,
                    )?;
            }
        } else if receipt.is_none() && !cleanup_completed {
            // A launcher failure without a proven pre-spawn rejection may already have caused
            // effects. The registration scan cannot prove process quiescence in this case.
            self.recorder
                .record_external_process_unknown_dirty_with_metadata(
                    &self.workspace_root,
                    process_name,
                    ToolEffect::Unknown,
                    metadata,
                )?;
        }
        // Publish stop only after the actual after-scan is durably accounted for. An audit failure
        // keeps the earlier pending generation visible instead of manufacturing a clean result.
        self.append(
            if cleanup_completed {
                ExtensionProcessLifecycleStatus::Stopped
            } else {
                ExtensionProcessLifecycleStatus::StopUnconfirmed
            },
            receipt,
        )
    }
}
