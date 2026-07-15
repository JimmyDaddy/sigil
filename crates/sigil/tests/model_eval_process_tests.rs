use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

#[test]
fn hidden_model_eval_process_runs_scripted_production_tool_path() -> Result<()> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (base_url, server) = spawn_scripted_deepseek_server(Arc::clone(&requests))?;
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_model_eval_config(&config_path, &base_url)?;
    let output_dir = temp.path().join("campaign");
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("bash")
        .arg(repo_root.join("scripts/run-evals.sh"))
        .args([
            "--model",
            "--config",
            config_path.to_str().context("config path is not UTF-8")?,
            "--case",
            "small-code-edit",
            "--repetitions",
            "1",
            "--max-cost-usd",
            "0.50",
            "--timeout-secs",
            "30",
            "--output-dir",
            output_dir.to_str().context("output path is not UTF-8")?,
        ])
        .current_dir(&repo_root)
        .env("SIGIL_BIN", env!("CARGO_BIN_EXE_sigil"))
        .env("SIGIL_API_KEY", "loopback-model-eval-key")
        .output()?;
    server
        .join()
        .map_err(|_| anyhow::anyhow!("loopback provider thread panicked"))??;

    if !output.status.success() {
        bail!(
            "model eval process failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for artifact in ["results.jsonl", "manifest.json", "summary.md"] {
        assert!(output_dir.join(artifact).is_file(), "missing {artifact}");
    }
    let manifest: sigil_kernel::ModelEvalReportManifestV3 =
        serde_json::from_slice(&fs::read(output_dir.join("manifest.json"))?)?;
    assert_eq!(manifest.report_schema_version, 3);
    assert_eq!(manifest.provider_admitted_repetitions, 1);
    assert_eq!(manifest.accepted_repetitions, 1);
    assert_eq!(
        manifest.trend_buckets[0].eligibility,
        sigil_kernel::ModelEvalTrendEligibility::SmokeOnly
    );
    let results = fs::read_to_string(output_dir.join("results.jsonl"))?;
    assert!(results.contains(r#""verification_verdict":"passed""#));
    assert!(results.contains(r#""tool_name":"edit_file""#));
    assert!(!results.contains("loopback-model-eval-key"));

    let requests = requests.lock().expect("request capture lock");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains(r#""name":"read_file""#));
    assert!(requests[0].contains(r#""name":"edit_file""#));
    assert!(!requests[0].contains(r#""name":"bash""#));
    assert!(!requests[0].contains("websearch"));
    Ok(())
}

#[test]
fn hidden_model_eval_process_rejects_missing_credential_before_provider_io() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_model_eval_config(&config_path, &base_url)?;
    let output_dir = temp.path().join("campaign");
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(env!("CARGO_BIN_EXE_sigil"))
        .args([
            "--config",
            config_path.to_str().context("config path is not UTF-8")?,
            "model-eval",
            "--case",
            "small-code-edit",
            "--repetitions",
            "1",
            "--max-cost-usd",
            "0.50",
            "--timeout-secs",
            "10",
            "--output-dir",
            output_dir.to_str().context("output path is not UTF-8")?,
        ])
        .current_dir(&repo_root)
        .env_remove("SIGIL_API_KEY")
        .env_remove("SIGIL_BASE_URL")
        .env_remove("SIGIL_BETA_BASE_URL")
        .env_remove("SIGIL_ANTHROPIC_BASE_URL")
        .output()?;

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("model eval acceptance failed")
            || String::from_utf8_lossy(&output.stderr).contains("credential")
            || String::from_utf8_lossy(&output.stderr).contains("API key")
    );
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    Ok(())
}

fn write_model_eval_config(path: &Path, base_url: &str) -> Result<()> {
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
mode = "auto-edit"

[providers.deepseek]
base_url = "{base_url}"
beta_base_url = "{base_url}"
anthropic_base_url = "{base_url}"
"#
        ),
    )?;
    Ok(())
}

fn spawn_scripted_deepseek_server(
    requests: Arc<Mutex<Vec<String>>>,
) -> Result<(String, thread::JoinHandle<Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        for response_index in 0..2 {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            bail!("timed out waiting for model eval provider request");
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            let request = read_http_request(&mut stream)?;
            requests.lock().expect("request capture lock").push(request);
            let body = if response_index == 0 {
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,",
                    "\"id\":\"call-edit\",\"function\":{\"name\":\"edit_file\",",
                    "\"arguments\":\"{\\\"path\\\":\\\"src/lib.rs\\\",",
                    "\\\"old_text\\\":\\\"left + right\\\",",
                    "\\\"new_text\\\":\\\"left * right\\\"}\"}}]},",
                    "\"finish_reason\":\"tool_calls\"}],",
                    "\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,",
                    "\"prompt_cache_hit_tokens\":0,\"prompt_cache_miss_tokens\":10}}\n\n",
                    "data: [DONE]\n\n"
                )
            } else {
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"fixed\"},",
                    "\"finish_reason\":\"stop\"}],",
                    "\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":2,",
                    "\"prompt_cache_hit_tokens\":0,\"prompt_cache_miss_tokens\":20}}\n\n",
                    "data: [DONE]\n\n"
                )
            };
            write_sse_response(&mut stream, body)?;
        }
        Ok(())
    });
    Ok((format!("http://{address}"), server))
}

fn read_http_request(stream: &mut TcpStream) -> Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if request_is_complete(&bytes) || bytes.len() >= 128 * 1024 {
            break;
        }
    }
    String::from_utf8(bytes).context("provider request is not UTF-8")
}

fn request_is_complete(bytes: &[u8]) -> bool {
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

fn write_sse_response(stream: &mut TcpStream, body: &str) -> Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}
