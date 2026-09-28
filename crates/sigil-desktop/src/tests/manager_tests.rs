use super::*;

#[test]
fn workspace_open_request_debug_redacts_every_native_path() {
    let request = DesktopWorkspaceOpenRequest::new(
        DesktopLaunchRequest::new(
            "/private/canary/sigil",
            "/private/canary/sigil.toml",
            "/private/canary/workspace",
        ),
        "workspace",
    );
    let debug = format!("{request:?}");

    assert!(!debug.contains("/private/canary"));
    assert!(debug.contains("<local path>"));
}

#[test]
fn display_name_validation_rejects_empty_control_and_oversized_values() {
    assert!(validate_display_name("workspace").is_ok());
    assert!(validate_display_name(" ").is_err());
    assert!(validate_display_name("bad\nname").is_err());
    assert!(validate_display_name(&"x".repeat(161)).is_err());
}

#[test]
fn discarded_open_ticket_releases_only_its_lifecycle_markers() {
    let manager = DesktopWorkspaceManager::default();
    let canonical_root = PathBuf::from("/private/canary/workspace");
    {
        let mut state = manager.lock_state();
        state.opening_roots.insert(canonical_root.clone());
    }
    let ticket = DesktopWorkspaceOpenTicket {
        canonical_root: canonical_root.clone(),
        display_name: "workspace".to_owned(),
        launch: DesktopLaunchRequest::with_implicit_user_config(
            "/private/canary/sigil",
            &canonical_root,
        ),
        existing: None,
    };

    manager.discard_open_ticket(&ticket);

    let state = manager.lock_state();
    assert!(!state.opening_roots.contains(&canonical_root));
    assert!(state.workspaces.is_empty());
}

#[test]
fn workspace_manager_lists_without_mutable_access() {
    let manager = DesktopWorkspaceManager::default();

    assert!(
        manager
            .list()
            .expect("empty manager should list")
            .is_empty()
    );
}

#[cfg(unix)]
mod lifecycle {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(start_paused: bool, stop_paused: bool) -> Self {
            let root = std::env::temp_dir().join(format!("desktop-owner-{}", Uuid::new_v4()));
            std::fs::create_dir(&root).expect("fixture directory");
            let binary = root.join("server.py");
            std::fs::write(&binary, include_str!("fixtures/workspace_server.py"))
                .expect("fixture server");
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))
                .expect("executable");
            let fixture = Self { root };
            if !start_paused {
                fixture.release("allow-start");
            }
            if !stop_paused {
                fixture.release("allow-stop");
            }
            fixture
        }

        fn request(&self) -> DesktopWorkspaceOpenRequest {
            DesktopWorkspaceOpenRequest::new(
                DesktopLaunchRequest::with_implicit_user_config(
                    self.root.join("server.py"),
                    &self.root,
                ),
                "fixture",
            )
        }

        fn release(&self, name: &str) {
            std::fs::write(self.root.join(name), "").expect("release fixture barrier");
        }

        async fn wait(&self, name: &str) {
            tokio::time::timeout(Duration::from_secs(15), async {
                while !tokio::fs::try_exists(self.root.join(name))
                    .await
                    .expect("barrier status")
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("fixture barrier reached");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn manager() -> DesktopWorkspaceManager {
        DesktopWorkspaceManager::new(DesktopLauncher::with_timeouts(
            Duration::from_secs(10),
            Duration::from_secs(10),
        ))
    }

    async fn wait_closing(manager: &DesktopWorkspaceManager) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !manager.lock_state().closing {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("close admission barrier");
    }

    #[tokio::test]
    async fn close_all_waits_for_cancelled_open_caller_and_reaps_late_ready() {
        let fixture = Fixture::new(true, false);
        let manager = manager();
        let opening = tokio::spawn({
            let manager = manager.clone();
            let request = fixture.request();
            async move { manager.open(request).await }
        });
        fixture.wait("starts").await;
        opening.abort();
        let _ = opening.await;
        let closing = tokio::spawn({
            let manager = manager.clone();
            async move { manager.close_all().await }
        });
        wait_closing(&manager).await;
        assert!(!closing.is_finished());
        assert!(matches!(
            manager.open(fixture.request()).await,
            Err(DesktopWorkspaceManagerError::WorkspaceOperationInProgress)
        ));
        fixture.release("allow-start");
        closing
            .await
            .expect("close task")
            .into_iter()
            .for_each(|(_, result)| {
                result.expect("shutdown");
            });
        fixture.wait("stopped").await;
        assert!(manager.list().expect("list after close").is_empty());
        assert!(manager.lock_state().operations.is_empty());
    }

    #[tokio::test]
    async fn closing_root_cannot_reopen_but_other_workspaces_remain_available() {
        let first = Fixture::new(false, true);
        let second = Fixture::new(false, false);
        let manager = manager();
        let opened = manager.open(first.request()).await.expect("open first");
        let closing = tokio::spawn({
            let manager = manager.clone();
            let id = opened.id.clone();
            async move { manager.close(&id).await }
        });
        first.wait("stopping").await;
        assert!(matches!(
            manager.open(first.request()).await,
            Err(DesktopWorkspaceManagerError::WorkspaceOperationInProgress)
        ));
        assert!(matches!(
            manager.restart(&opened.id).await,
            Err(DesktopWorkspaceManagerError::WorkspaceOperationInProgress)
        ));
        let other = manager
            .open(second.request())
            .await
            .expect("independent open");
        assert!(manager.client(&other.id).is_ok());
        assert_eq!(
            std::fs::read_to_string(first.root.join("starts"))
                .expect("starts")
                .lines()
                .count(),
            1
        );
        first.release("allow-stop");
        closing.await.expect("close task").expect("first shutdown");
        manager.close(&other.id).await.expect("second shutdown");
    }

    #[tokio::test]
    async fn identity_collision_cleanup_keeps_other_workspace_operation_reserved() {
        let first = Fixture::new(false, false);
        let second = Fixture::new(false, false);
        let manager = manager();
        let opened = manager.open(first.request()).await.expect("initial open");
        std::fs::remove_file(first.root.join("allow-start")).expect("pause restart");
        std::fs::write(second.root.join("identity"), &opened.id).expect("collision identity");
        let restarting = tokio::spawn({
            let manager = manager.clone();
            let id = opened.id.clone();
            async move { manager.restart(&id).await }
        });
        first.wait("stopping").await;
        assert!(matches!(
            manager.open(second.request()).await,
            Err(DesktopWorkspaceManagerError::IdentityCollision)
        ));
        assert!(matches!(
            manager.restart(&opened.id).await,
            Err(DesktopWorkspaceManagerError::WorkspaceOperationInProgress)
        ));
        first.release("allow-start");
        assert_eq!(
            restarting.await.expect("restart task").expect("restart").id,
            opened.id
        );
        assert!(
            manager
                .close_all()
                .await
                .iter()
                .all(|(_, result)| result.is_ok())
        );
    }

    #[tokio::test]
    async fn close_all_waits_for_restart_and_does_not_publish_its_new_process() {
        let fixture = Fixture::new(false, false);
        let manager = manager();
        let opened = manager.open(fixture.request()).await.expect("initial open");
        std::fs::remove_file(fixture.root.join("allow-start")).expect("pause restart");
        let restarting = tokio::spawn({
            let manager = manager.clone();
            async move { manager.restart(&opened.id).await }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if tokio::fs::read_to_string(fixture.root.join("starts"))
                    .await
                    .expect("starts")
                    .lines()
                    .count()
                    == 2
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("restart reached startup barrier");
        let closing = tokio::spawn({
            let manager = manager.clone();
            async move { manager.close_all().await }
        });
        wait_closing(&manager).await;
        assert!(!closing.is_finished());
        fixture.release("allow-start");
        assert!(matches!(
            restarting.await.expect("restart task"),
            Err(DesktopWorkspaceManagerError::WorkspaceOperationInProgress)
        ));
        assert!(
            closing
                .await
                .expect("close all")
                .iter()
                .all(|(_, result)| result.is_ok())
        );
        assert!(manager.list().expect("list").is_empty());
        assert!(manager.lock_state().opening_roots.is_empty());
    }
}
