//! Shipping registration must never make the terminal manager select a direct process fallback.

#[cfg(unix)]
mod unix {
    use std::{fs, path::Path, sync::Arc};

    use anyhow::Result;
    use serde_json::json;
    use sigil_kernel::{ToolCall, ToolContext, ToolRegistry};
    use sigil_tools_builtin::{
        BuiltinToolPaths, ScratchDeleteOutcome, ScratchGcConfig, ScratchGcReport,
        ScratchNamespaceControl, ScratchNamespaceProvider, ScratchNamespaceProviderLease,
        ScratchQuota, ScratchUsage, SessionScratchProvision, TerminalExecutionConfig,
        TerminalProcessManager, TerminalStartRequest, UnavailableManagedCommandExecutionPortV1,
        register_builtin_tools_with_managed_execution_and_terminal_config,
    };

    #[derive(Debug)]
    struct NoopLease;

    impl ScratchNamespaceProviderLease for NoopLease {}

    #[derive(Debug)]
    struct TestScratchProvider {
        root: std::path::PathBuf,
    }

    impl ScratchNamespaceProvider for TestScratchProvider {
        fn acquire(&self, _session_key: &str) -> Result<Box<dyn ScratchNamespaceProviderLease>> {
            Ok(Box::new(NoopLease))
        }

        fn session_scratch_dir(&self, session_scope_id: Option<&str>) -> std::path::PathBuf {
            self.root
                .join("sessions")
                .join(sigil_tools_builtin::session_scratch_key(session_scope_id))
        }

        fn ensure_session_scratch(
            &self,
            session_scope_id: Option<&str>,
            _quota: &ScratchQuota,
        ) -> Result<SessionScratchProvision> {
            let dir = self.session_scratch_dir(session_scope_id);
            fs::create_dir_all(&dir)?;
            Ok(SessionScratchProvision {
                dir,
                usage: ScratchUsage::default(),
            })
        }

        fn measure_scratch_usage(&self, _session_key: &str) -> Result<ScratchUsage> {
            Ok(ScratchUsage::default())
        }

        fn gc_scratch_namespaces(
            &self,
            _control: &ScratchNamespaceControl,
            _config: &ScratchGcConfig,
            _now_ms: u64,
        ) -> Result<ScratchGcReport> {
            Ok(ScratchGcReport::default())
        }

        fn delete_session_scratch_namespace(
            &self,
            _session_scope_id: Option<&str>,
            _control: &ScratchNamespaceControl,
        ) -> Result<ScratchDeleteOutcome> {
            Ok(ScratchDeleteOutcome::NotPresent)
        }
    }

    fn call(pty: bool) -> Result<ToolCall> {
        Ok(ToolCall {
            id: format!("terminal-start-{pty}"),
            name: "terminal_start".to_owned(),
            args_json: serde_json::to_string(&json!({
                "task_id": format!("fail-closed-{pty}"),
                "command": "touch spawned-by-terminal",
                "mode": if pty { "interactive" } else { "background" },
                "pty": pty,
            }))?,
        })
    }

    fn register(workspace: &Path) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        let scratch = ScratchNamespaceControl::from_provider(Arc::new(TestScratchProvider {
            root: workspace.join("scratch"),
        }));
        register_builtin_tools_with_managed_execution_and_terminal_config(
            &mut registry,
            BuiltinToolPaths::workspace_defaults(workspace),
            Arc::new(UnavailableManagedCommandExecutionPortV1),
            TerminalExecutionConfig::default(),
            None,
            Some(scratch),
        );
        registry
    }

    async fn assert_fail_closed(pty: bool) -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let registry = register(workspace.path());
        let error = registry
            .execute(
                ToolContext::new(workspace.path().to_path_buf(), 5),
                call(pty)?,
            )
            .await
            .expect_err("shipping terminal registration must reject unavailable managed launch");

        assert!(error.to_string().contains("managed terminal launch failed"));
        assert!(matches!(
            error.downcast_ref::<sigil_kernel::managed_execution::ManagedExecutionErrorV1>(),
            Some(sigil_kernel::managed_execution::ManagedExecutionErrorV1::ProviderUnavailable)
        ));
        assert!(
            !workspace.path().join("spawned-by-terminal").exists(),
            "terminal command must not be spawned without an authority port"
        );
        Ok(())
    }

    #[tokio::test]
    async fn shipping_default_registration_rejects_non_pty_without_spawning() -> Result<()> {
        assert_fail_closed(false).await
    }

    #[tokio::test]
    async fn shipping_default_registration_rejects_pty_without_spawning() -> Result<()> {
        assert_fail_closed(true).await
    }

    async fn assert_public_manager_constructor_fails_closed(pty: bool) -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let manager = TerminalProcessManager::new(workspace.path())?;
        let request = TerminalStartRequest {
            task_id: None,
            command: "touch spawned-by-public-manager".to_owned(),
            cwd: None,
            shell: None,
            env: Default::default(),
        };
        let error = if pty {
            manager.start_pty(request, None).await
        } else {
            manager.start(request).await
        }
        .expect_err("normal-build public manager constructor must require managed authority");
        assert!(error.to_string().contains("managed terminal launch failed"));
        assert!(matches!(
            error.downcast_ref::<sigil_kernel::managed_execution::ManagedExecutionErrorV1>(),
            Some(sigil_kernel::managed_execution::ManagedExecutionErrorV1::ProviderUnavailable)
        ));
        assert!(!workspace.path().join("spawned-by-public-manager").exists());
        Ok(())
    }

    #[tokio::test]
    async fn normal_build_public_manager_rejects_non_pty_without_spawning() -> Result<()> {
        assert_public_manager_constructor_fails_closed(false).await
    }

    #[tokio::test]
    async fn normal_build_public_manager_rejects_pty_without_spawning() -> Result<()> {
        assert_public_manager_constructor_fails_closed(true).await
    }
}
