use super::*;

fn assert_competing_workspace_rejected(workspace: &Path, config: &Path, home: &Path, token: &str) {
    let stdout_path = workspace.join("competing.stdout");
    let stderr_path = workspace.join("competing.stderr");
    let mut command = Command::new(env!("CARGO_BIN_EXE_sigil"));
    common::isolated_child_environment_for_home(home)
        .expect("isolated competitor")
        .apply_to_command(&mut command);
    let mut child = command
        .current_dir(workspace)
        .env("SIGIL_HTTP_TOKEN", token)
        .arg("--config")
        .arg(config)
        .arg("serve")
        .stdout(File::create(&stdout_path).expect("stdout"))
        .stderr(File::create(&stderr_path).expect("stderr"))
        .spawn()
        .expect("competing serve");
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().expect("competitor status") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("stop unexpected second controller");
            child.wait().expect("reap unexpected controller");
            panic!("same workspace admitted a second live server");
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        !status.success(),
        "competing controller must fail before readiness"
    );
    let stdout = fs::read_to_string(stdout_path).expect("startup output");
    let stderr = fs::read_to_string(stderr_path).expect("startup diagnostic");
    assert!(
        !stdout.contains("bind: "),
        "competing server must not publish readiness"
    );
    assert!(
        stderr.contains("failed to acquire durable lease"),
        "workspace protocol ownership remains enforced: {stderr}"
    );
}

#[test]
fn desktop_workspace_servers_share_config_without_competing_for_authority() {
    let fixture = tempfile::tempdir().expect("isolated fixture");
    let config = fixture.path().join("sigil.toml");
    write_config(&config, "http://127.0.0.1:1");
    let home = fixture.path().join("user");
    let a = fixture.path().join("workspace-a");
    let b = fixture.path().join("workspace-b");
    for workspace in [&a, &b] {
        fs::create_dir(workspace).expect("workspace");
    }
    let token = "workspace-authority-test";
    let first = spawn_serve_with_home(&a, &config, token, Some(&home));
    let (status, body) = http_request(
        first.address,
        "POST",
        "/sessions",
        Some(token),
        Some(r#"{"label":"retained legacy session"}"#),
    );
    assert_eq!(status, 201, "legacy session: {body}");
    let legacy: serde_json::Value = serde_json::from_str(&body).expect("legacy session");
    let legacy_id = legacy["durable_session_scope_id"].clone();
    let original_realm = fs::read_dir(home.join(".sigil/authority-bootstrap-v1"))
        .expect("authority namespace")
        .map(|entry| entry.expect("entry").path())
        .find(|path| path.is_dir())
        .expect("original realm");
    let pointer =
        fs::read(original_realm.join(".sigil-cutover-manifest.json")).expect("original pointer");
    // Reconstruct the shipping pre-upgrade layout while its genuine owner remains alive.
    // A new workspace must not bind shared history before that owner has settled.
    fs::remove_file(original_realm.join("workspace-binding-v1.json"))
        .expect("legacy binding absence");
    let inventory =
        fs::read(original_realm.join("process-inventory.json")).expect("legacy inventory");
    let wrong_first = spawn_serve_with_home(&b, &config, token, Some(&home));
    let (status, body) = http_request(
        wrong_first.address,
        "POST",
        "/sessions",
        Some(token),
        Some(r#"{"label":"unproven legacy workspace"}"#),
    );
    assert_eq!(status, 409, "live legacy owner remains exclusive: {body}");
    let error: serde_json::Value = serde_json::from_str(&body).expect("typed recovery");
    let recovery = &error["error"]["route_recovery"];
    assert_eq!(recovery["code"], "authority_unavailable");
    assert_eq!(recovery["retryable"], false);
    assert!(stop_serve(wrong_first).status.success());
    assert_eq!(
        fs::read(original_realm.join("process-inventory.json"))
            .expect("unchanged legacy inventory"),
        inventory
    );
    assert!(!original_realm.join("workspace-binding-v1.json").exists());
    assert_eq!(
        fs::read(original_realm.join(".sigil-cutover-manifest.json")).expect("same pointer"),
        pointer
    );
    assert_eq!(
        http_request(first.address, "GET", "/session-catalog", Some(token), None).0,
        200,
        "the original live owner must remain usable"
    );
    assert!(stop_serve(first).status.success());
    let source = fs::read_dir(fixture.path().join("state/managed/session-log"))
        .expect("legacy source root")
        .map(|entry| entry.expect("entry").path().join("records.jsonl"))
        .find(|path| path.is_file())
        .expect("legacy source");
    let source_bytes = fs::read(&source).expect("legacy bytes");
    let original_pointer: serde_json::Value = serde_json::from_slice(&pointer).expect("pointer");
    let replacement = fixture.path().join("replacement.toml");
    let updated = fs::read_to_string(&config)
        .expect("config")
        .replace("model = \"gpt-4.1\"", "model = \"updated-model\"");
    fs::write(&replacement, updated).expect("replacement config");
    fs::rename(&replacement, &config).expect("atomic config replacement");
    let first = spawn_serve_with_home(&a, &config, token, Some(&home));
    let current_pointer: serde_json::Value = serde_json::from_slice(
        &fs::read(original_realm.join(".sigil-cutover-manifest.json")).expect("updated pointer"),
    )
    .expect("updated manifest");
    assert_eq!(
        current_pointer["authority_generation_digest"],
        original_pointer["authority_generation_digest"],
        "config replacement must preserve the original authority identity"
    );
    assert_eq!(
        current_pointer["application_generation"].as_u64(),
        original_pointer["application_generation"]
            .as_u64()
            .map(|value| value + 1)
    );
    let second = spawn_serve_with_home(&b, &config, token, Some(&home));
    let mut durable_ids = Vec::new();
    for server in [&first, &second] {
        let (status, body) = http_request(
            server.address,
            "POST",
            "/sessions",
            Some(token),
            Some(r#"{"label":"workspace authority"}"#),
        );
        assert_eq!(status, 201, "independent workspace session: {body}");
        let created: serde_json::Value = serde_json::from_str(&body).expect("session");
        durable_ids.push(created["durable_session_scope_id"].clone());
    }
    assert_eq!(
        fs::read(&source).expect("legacy source retained"),
        source_bytes
    );
    let (_, body) = http_request(second.address, "GET", "/session-catalog", Some(token), None);
    let catalog: sigil_desktop::DesktopSessionCatalogPage = serde_json::from_str(&body)
        .expect("real HTTP catalog must decode through native allowlist");
    let legacy_notice = catalog
        .legacy_shared_history
        .expect("legacy history must remain discoverable");
    assert_eq!(
        legacy_notice.workspace_id,
        sigil_runtime::workspace_id_for_root(&a)
    );
    assert_eq!(legacy_notice.workspace_label, "workspace-a");
    assert_eq!(
        legacy_notice.source_state,
        sigil_desktop::DesktopLegacySharedSessionHistorySourceState::Present
    );
    let (_, body) = http_request(first.address, "GET", "/session-catalog", Some(token), None);
    let catalog: serde_json::Value = serde_json::from_str(&body).expect("legacy catalog");
    assert!(
        catalog["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|row| row["session_id"] == legacy_id)
    );
    assert_competing_workspace_rejected(&a, &config, &home, token);
    // The refused competitor must not invalidate either existing controller.
    for server in [&first, &second] {
        assert_eq!(
            http_request(server.address, "GET", "/session-catalog", Some(token), None).0,
            200
        );
        let (status, body) = http_request(
            server.address,
            "POST",
            "/sessions",
            Some(token),
            Some(r#"{"label":"original owner still usable"}"#),
        );
        assert_eq!(
            status, 201,
            "refused competitor must not invalidate the original owner: {body}"
        );
    }
    assert!(stop_serve(second).status.success());
    assert!(stop_serve(first).status.success());
    for (workspace, expected_id) in [&a, &b].into_iter().zip(durable_ids) {
        let reopened = spawn_serve_with_home(workspace, &config, token, Some(&home));
        let (status, body) = http_request(
            reopened.address,
            "GET",
            "/session-catalog",
            Some(token),
            None,
        );
        assert_eq!(status, 200, "cold catalog: {body}");
        let catalog: serde_json::Value = serde_json::from_str(&body).expect("catalog");
        let rows = catalog["entries"].as_array().expect("entries");
        let mut expected_ids = vec![expected_id];
        if workspace == &a {
            expected_ids.push(legacy_id.clone());
        }
        for expected_id in expected_ids {
            let retained = rows
                .iter()
                .find(|row| row["session_id"] == expected_id)
                .expect("own durable session remains visible");
            let request =
                serde_json::json!({"session_ref": retained["session_ref"], "session_id": expected_id})
                    .to_string();
            let (status, body) = http_request(
                reopened.address,
                "POST",
                "/sessions/open",
                Some(token),
                Some(&request),
            );
            assert_eq!(status, 200, "cold reopen: {body}");
            let session: serde_json::Value = serde_json::from_str(&body).expect("reopened session");
            assert_eq!(session["durable_session_scope_id"], expected_id);
        }
        assert!(stop_serve(reopened).status.success());
    }
}
