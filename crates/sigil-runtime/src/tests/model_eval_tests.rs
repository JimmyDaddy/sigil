use std::{
    env, fs,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use sigil_kernel::{
    ControlEntry, DisclosurePresentationError, DisclosurePresentationReceipt,
    EgressDisclosurePresenter, JsonlSessionStore, PreEgressDisclosure, ReceiptStatus, Session,
    VerificationVerdict, write_file_with_mutation,
};
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use crate::{
    application_run::ApplicationRunServices,
    model_eval::{
        ModelEvalCampaignRequest, ModelEvalCostConfidence, ModelEvalRunExecutionStatus,
        load_model_eval_fixture, materialize_model_eval_fixture, run_model_eval_campaign,
        verify_model_eval_run, write_isolated_model_eval_config,
    },
};

struct RejectingPresenter;

#[async_trait::async_trait]
impl EgressDisclosurePresenter for RejectingPresenter {
    async fn present(
        &self,
        _disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        Err(DisclosurePresentationError::SinkClosed)
    }
}

fn fixture_root(id: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../dev/evals/model-fixtures")
        .join(id)
}

#[test]
fn committed_model_eval_fixtures_load_and_materialize() {
    for id in [
        "small-doc-edit",
        "small-code-edit",
        "stale-after-write",
        "workspace-trust",
        "sandbox-denial",
    ] {
        let fixture = load_model_eval_fixture(fixture_root(id)).expect("fixture should load");
        assert_eq!(fixture.manifest.id, id);
        let temp = tempdir().expect("temp dir");
        let destination = temp.path().join("workspace");
        let materialized =
            materialize_model_eval_fixture(&fixture, &destination).expect("materialize fixture");
        assert_eq!(materialized.fixture_id, id);
        assert!(materialized.tree_digest.starts_with("sha256:"));
        assert!(destination.join("Cargo.toml").is_file());
        assert!(!materialized.tool_scope.allows("bash"));
        assert!(!materialized.tool_scope.allows("websearch"));
    }
}

#[test]
fn model_eval_fixture_rejects_digest_drift() {
    let source = fixture_root("small-code-edit");
    let temp = tempdir().expect("temp dir");
    copy_directory(&source, temp.path());
    fs::write(
        temp.path().join("files/src/lib.rs"),
        "pub fn value() -> u32 { 9 }\n",
    )
    .expect("drift source");

    let error = load_model_eval_fixture(temp.path()).expect_err("digest drift must fail");
    assert!(error.to_string().contains("file sha256 mismatch"));
}

#[test]
fn model_eval_fixture_rejects_unknown_fields_and_tools() {
    let source = fixture_root("small-code-edit");
    let temp = tempdir().expect("temp dir");
    copy_directory(&source, temp.path());
    let manifest_path = temp.path().join("fixture.toml");
    let manifest = fs::read_to_string(&manifest_path).expect("read manifest");
    fs::write(
        &manifest_path,
        manifest.replace(
            "allowed_tools = [\"read_file\", \"edit_file\"]",
            "allowed_tools = [\"read_file\", \"bash\"]\nunknown = true",
        ),
    )
    .expect("write manifest");

    let error = load_model_eval_fixture(temp.path()).expect_err("unknown field must fail");
    assert!(error.to_string().contains("failed to parse"));
}

#[cfg(unix)]
#[test]
fn model_eval_fixture_rejects_symlinked_sources() {
    use std::os::unix::fs::symlink;

    let source = fixture_root("small-code-edit");
    let temp = tempdir().expect("temp dir");
    copy_directory(&source, temp.path());
    let file = temp.path().join("files/src/lib.rs");
    fs::remove_file(&file).expect("remove copied source");
    symlink("../../prompt.txt", &file).expect("create symlink");

    let error = load_model_eval_fixture(temp.path()).expect_err("symlink must fail");
    assert!(error.to_string().contains("not a regular file"));
}

#[test]
fn model_eval_materializer_refuses_existing_destination() {
    let fixture = load_model_eval_fixture(fixture_root("small-doc-edit")).expect("load fixture");
    let temp = tempdir().expect("temp dir");
    let error = materialize_model_eval_fixture(&fixture, temp.path())
        .expect_err("existing destination must fail");
    assert!(error.to_string().contains("already exists"));
}

#[test]
fn isolated_model_eval_config_removes_secrets_and_external_surfaces() {
    let fixture = load_model_eval_fixture(fixture_root("small-code-edit")).expect("load fixture");
    let temp = tempdir().expect("temp dir");
    let run_root = temp.path().join("run");
    fs::create_dir(&run_root).expect("run root");
    let materialized = materialize_model_eval_fixture(&fixture, run_root.join("workspace"))
        .expect("materialize fixture");
    let source_config = temp.path().join("source.toml");
    write_source_config(&source_config, "http://127.0.0.1:9", "auto-edit");

    let isolated = write_isolated_model_eval_config(&source_config, &materialized, &run_root)
        .expect("write isolated config");
    let rendered = fs::read_to_string(&isolated.config_path).expect("read isolated config");

    assert!(!rendered.contains("inline-secret-must-not-copy"));
    assert!(!rendered.to_ascii_lowercase().contains("api_key"));
    assert!(rendered.contains("enabled = false"));
    assert!(rendered.contains(&materialized.workspace_root.display().to_string()));
    assert!(isolated.session_path.starts_with(&run_root));

    let second_run_root = temp.path().join("run-2");
    fs::create_dir(&second_run_root).expect("second run root");
    let second_materialized =
        materialize_model_eval_fixture(&fixture, second_run_root.join("workspace"))
            .expect("materialize second fixture");
    let second =
        write_isolated_model_eval_config(&source_config, &second_materialized, &second_run_root)
            .expect("write second isolated config");
    assert_eq!(isolated.config_digest, second.config_digest);
    assert_ne!(
        isolated.isolated_config_digest,
        second.isolated_config_digest
    );
}

#[test]
fn isolated_model_eval_config_requires_noninteractive_write_permission() {
    let fixture = load_model_eval_fixture(fixture_root("small-doc-edit")).expect("load fixture");
    let temp = tempdir().expect("temp dir");
    let run_root = temp.path().join("run");
    fs::create_dir(&run_root).expect("run root");
    let materialized = materialize_model_eval_fixture(&fixture, run_root.join("workspace"))
        .expect("materialize fixture");
    let source_config = temp.path().join("source.toml");
    write_source_config(&source_config, "http://127.0.0.1:9", "manual");

    let error = write_isolated_model_eval_config(&source_config, &materialized, &run_root)
        .expect_err("manual config without exact tool grants must fail");
    assert!(error.to_string().contains("controlled workspace edits"));
}

#[test]
fn model_eval_campaign_uses_production_run_constraints_and_budget() {
    let _env_lock = crate::test_env::lock();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_deepseek_eval_server(Arc::clone(&requests))
            .await
            .expect("spawn server");
        let _api_key = EnvironmentGuard::set("SIGIL_API_KEY", "model-eval-test-key");
        let _base_url = EnvironmentGuard::set("SIGIL_BASE_URL", &base_url);
        let _beta_url = EnvironmentGuard::set("SIGIL_BETA_BASE_URL", &base_url);
        let _anthropic_url = EnvironmentGuard::set("SIGIL_ANTHROPIC_BASE_URL", &base_url);
        let temp = tempdir().expect("temp dir");
        let config_path = temp.path().join("source.toml");
        write_source_config(&config_path, &base_url, "auto-edit");
        let services = ApplicationRunServices::new(Arc::new(RejectingPresenter));

        let campaign = run_model_eval_campaign(
            ModelEvalCampaignRequest {
                config_path,
                fixture_roots: vec![fixture_root("small-code-edit")],
                repetitions: 2,
                max_cost_microusd: 500_000,
                campaign_timeout: Duration::from_secs(10),
                output_dir: temp.path().join("campaign"),
            },
            &services,
        )
        .await
        .expect("run campaign");

        assert_eq!(campaign.planned_runs, 2);
        assert_eq!(campaign.runs.len(), 2);
        assert_eq!(
            campaign.runs[0].status,
            ModelEvalRunExecutionStatus::Completed
        );
        assert_eq!(
            campaign.runs[0].cost_confidence,
            ModelEvalCostConfidence::Reported
        );
        assert_eq!(
            campaign.runs[1].status,
            ModelEvalRunExecutionStatus::BudgetSkipped
        );
        assert!(campaign.runs[0].session_path.is_file());
        assert!(campaign.output_dir.join("results.jsonl").is_file());
        assert!(campaign.output_dir.join("manifest.json").is_file());
        assert!(campaign.output_dir.join("summary.md").is_file());
        let request = requests
            .lock()
            .expect("requests lock")
            .first()
            .cloned()
            .expect("provider request");
        assert!(request.contains(r#""max_tokens":4096"#));
        assert!(request.contains(r#""name":"read_file""#));
        assert!(request.contains(r#""name":"edit_file""#));
        assert!(!request.contains(r#""name":"bash""#));
        assert!(!request.contains("websearch"));
        assert_eq!(requests.lock().expect("requests lock").len(), 1);
    });
}

#[test]
fn model_eval_verification_records_pass_then_durable_stale_mutation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let fixture =
            load_model_eval_fixture(fixture_root("stale-after-write")).expect("load fixture");
        let temp = tempdir().expect("temp dir");
        let run_root = temp.path().join("run");
        fs::create_dir(&run_root).expect("run root");
        let materialized = materialize_model_eval_fixture(&fixture, run_root.join("workspace"))
            .expect("materialize fixture");
        let source_config = temp.path().join("source.toml");
        write_source_config(&source_config, "http://127.0.0.1:9", "auto-edit");
        let isolated = write_isolated_model_eval_config(&source_config, &materialized, &run_root)
            .expect("write isolated config");
        let store = JsonlSessionStore::new(&isolated.session_path).expect("session store");
        let mut session = Session::new(&isolated.provider, &isolated.model).with_store(store);
        session
            .append_control(ControlEntry::Note {
                kind: "model_eval_test".to_owned(),
                data: serde_json::json!({"phase": "model_completed"}),
            })
            .expect("initialize session");
        let source_path = materialized.workspace_root.join("src/lib.rs");
        let source = fs::read_to_string(&source_path).expect("read fixture source");
        let updated = source.replace("    1\n", "    2\n");
        let recorder = session
            .mutation_event_recorder()
            .expect("durable mutation recorder");
        write_file_with_mutation(
            Some(&recorder),
            &materialized.workspace_root,
            "model-edit",
            "src/lib.rs",
            &source_path,
            updated.as_bytes(),
        )
        .expect("record model mutation");
        drop(session);

        let verification = verify_model_eval_run(
            &materialized,
            &isolated.config_path,
            &isolated.session_path,
            &isolated.provider,
            &isolated.model,
            "run-stale",
        )
        .await
        .expect("verify fixture");

        assert_eq!(verification.verdict, VerificationVerdict::Stale);
        assert!(verification.post_run_mutation_recorded);
        assert_eq!(verification.receipts.len(), 1);
        assert_eq!(
            verification.receipts[0].receipt.check_status,
            ReceiptStatus::Succeeded
        );
        assert!(
            fs::read_to_string(materialized.workspace_root.join("README.md"))
                .expect("read mutated readme")
                .contains("fixture_generation = 2")
        );
        let reloaded = Session::load_from_store(
            &isolated.provider,
            &isolated.model,
            JsonlSessionStore::new(&isolated.session_path).expect("reopen store"),
        )
        .expect("reload session");
        assert!(reloaded.entries().iter().any(|entry| matches!(
            entry,
            sigil_kernel::SessionLogEntry::Control(ControlEntry::VerificationRecorded(_))
        )));
    });
}

fn copy_directory(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("fixture entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            fs::create_dir_all(&target).expect("copy directory");
            copy_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

fn write_source_config(path: &Path, base_url: &str, permission_mode: &str) {
    fs::write(
        path,
        format!(
            r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
max_turns = 12

[permission]
mode = "{permission_mode}"

[providers.deepseek]
base_url = "{base_url}"
beta_base_url = "{base_url}"
anthropic_base_url = "{base_url}"
api_key = "inline-secret-must-not-copy"
"#
        ),
    )
    .expect("write source config");
}

async fn spawn_deepseek_eval_server(requests: Arc<Mutex<Vec<String>>>) -> anyhow::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let count = socket.read(&mut chunk).await.unwrap_or_default();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
            if http_request_is_complete(&bytes) || bytes.len() >= 64 * 1024 {
                break;
            }
        }
        requests
            .lock()
            .expect("requests lock")
            .push(String::from_utf8_lossy(&bytes).into_owned());
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}],",
            "\"usage\":{\"prompt_tokens\":10000000,\"completion_tokens\":2,",
            "\"prompt_cache_hit_tokens\":0,\"prompt_cache_miss_tokens\":10000000}}\n\n",
            "data: [DONE]\n\n"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    Ok(format!("http://{address}"))
}

fn http_request_is_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    content_length.is_some_and(|length| bytes.len() >= header_end + 4 + length)
}

struct EnvironmentGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvironmentGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = env::var_os(name);
        // SAFETY: runtime tests serialize environment mutation through `test_env::lock`.
        unsafe { env::set_var(name, value) };
        Self { name, previous }
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => {
                // SAFETY: the same serialized test guard is still held during drop.
                unsafe { env::set_var(self.name, value) };
            }
            None => {
                // SAFETY: the same serialized test guard is still held during drop.
                unsafe { env::remove_var(self.name) };
            }
        }
    }
}
