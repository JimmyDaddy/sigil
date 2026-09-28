use super::*;

struct HeldProvider {
    base_url: String,
    received: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl HeldProvider {
    fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let base_url = format!("http://{}", listener.local_addr()?);
        listener.set_nonblocking(true)?;
        let received = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_received = Arc::clone(&received);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !worker_stop.load(Ordering::Acquire) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .expect("provider fixture stream should accept a read timeout");
                        let _ = read_http_message(&mut stream);
                        worker_received.store(true, Ordering::Release);
                        while !worker_stop.load(Ordering::Acquire) && Instant::now() < deadline {
                            thread::sleep(Duration::from_millis(5));
                        }
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            base_url,
            received,
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for HeldProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_plugin_review_serve_contract_uses_exact_session_without_idle_gate()
-> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let provider = HeldProvider::start()?;
    let config = workspace.path().join("sigil.toml");
    write_config(&config, &provider.base_url);
    fs::write(
        &config,
        fs::read_to_string(&config)?
            .replace("request_timeout_secs = 5", "request_timeout_secs = 30"),
    )?;
    let plugin_root = workspace.path().join(".sigil/plugins/review");
    fs::create_dir_all(&plugin_root)?;
    let manifest = plugin_root.join("plugin.toml");
    fs::write(
        &manifest,
        r#"id = "review"
name = "Review tools"
version = "1.0.0"
[[mcp_servers]]
name = "echo"
transport = "stdio"
command = "/bin/sh"
args = ["-c", "printf launched > must-not-start"]
startup = "lazy"
"#,
    )?;
    let manager = sigil_desktop::DesktopWorkspaceManager::default();
    let result: anyhow::Result<()> = async {
        let opened = manager
            .open(sigil_desktop::DesktopWorkspaceOpenRequest::new(
                sigil_desktop::DesktopLaunchRequest::new(
                    env!("CARGO_BIN_EXE_sigil"),
                    &config,
                    workspace.path(),
                ),
                "plugin review",
            ))
            .await?;
        let client = manager.client(&opened.id)?;
        let session = client
            .create_session(sigil_desktop::DesktopSessionCreateRequest {
                label: None,
                model_ref: None,
            })
            .await?;
        let catalog = client.plugin_catalog(&session.id).await?;
        assert_eq!(catalog.plugins.len(), 1);
        let plugin = catalog.plugins[0].clone();
        assert_eq!(plugin.trust, "needs_review");
        let public = serde_json::to_string(&catalog)?;
        for private in [
            plugin_root.to_str().expect("fixture path should be UTF-8"),
            "/bin/sh",
            "must-not-start",
        ] {
            assert!(!public.contains(private));
        }
        let run = client
            .start_run(
                &session.id,
                sigil_desktop::DesktopRunStartRequest {
                    prompt: "Wait for the provider response".to_owned(),
                    image_attachments: Vec::new(),
                    review_annotations: Vec::new(),
                    permission_mode: sigil_desktop::DesktopPermissionMode::ReadOnly,
                    model_ref: None,
                    model_selection_binding: None,
                    route_recovery_binding: None,
                    reasoning_effort: None,
                    reasoning_effort_binding: None,
                    skill_binding: None,
                    agent_binding: None,
                    task_continuation: None,
                },
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(15), async {
            while !provider.received.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        assert!(!client.run(&run.run.id).await?.status.is_terminal());
        for enabled in [true, false] {
            let receipt = client
                .command_conversation_recovery(
                    &session.id,
                    sigil_desktop::DesktopConversationRecoveryCommandAction::ReviewPlugin {
                        plugin_id: plugin.plugin_id.clone(),
                        manifest_hash: plugin.manifest_hash.clone(),
                        capability_digest: plugin.capability_digest.clone(),
                        enabled,
                    },
                )
                .await?;
            assert_eq!(
                receipt.plugin_review.as_ref().map(|value| value.enabled),
                Some(enabled)
            );
            assert!(
                !client.run(&run.run.id).await?.status.is_terminal(),
                "review must not cancel the active run"
            );
        }
        assert_eq!(
            client.plugin_catalog(&session.id).await?.plugins[0].trust,
            "disabled"
        );
        assert!(!plugin_root.join("must-not-start").exists());
        fs::write(
            &manifest,
            fs::read_to_string(&manifest)?.replace("1.0.0", "1.0.1"),
        )?;
        assert!(
            client
                .command_conversation_recovery(
                    &session.id,
                    sigil_desktop::DesktopConversationRecoveryCommandAction::ReviewPlugin {
                        plugin_id: plugin.plugin_id,
                        manifest_hash: plugin.manifest_hash,
                        capability_digest: plugin.capability_digest,
                        enabled: true,
                    }
                )
                .await
                .is_err()
        );
        assert_eq!(
            client.plugin_catalog(&session.id).await?.plugins[0].trust,
            "needs_review"
        );
        Ok(())
    }
    .await;
    let cleanup = manager.close_all().await;
    result?;
    anyhow::ensure!(
        cleanup.iter().all(|(_, result)| result.is_ok()),
        "serve cleanup failed: {cleanup:?}"
    );
    Ok(())
}
