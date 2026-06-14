use std::path::{Path, PathBuf};

#[cfg(not(test))]
use std::env;

use anyhow::Result;
use clap::{Parser, Subcommand};
use futures::StreamExt;
#[cfg(not(test))]
use sigil_kernel::preferred_config_path;
use sigil_kernel::{
    Agent, EventHandler, InteractionMode, JsonlSessionStore, ProviderChunk, RootConfig, RunEvent,
    Session, resolve_workspace_root,
};
use sigil_provider_deepseek::{
    DeepSeekFimCompletionRequest, DeepSeekPrefixCompletionRequest, DeepSeekProvider,
};
use sigil_runtime::doctor::{DoctorReport, build_doctor_report};

#[derive(Parser)]
#[command(name = "sigil")]
#[command(about = "TUI-first shell for Sigil")]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Run {
        prompt: String,
    },
    Doctor,
    #[command(hide = true)]
    Prefix {
        prompt: String,
        #[arg(long)]
        assistant_prefix: String,
        #[arg(long = "stop")]
        stop: Vec<String>,
        #[arg(long)]
        model: Option<String>,
    },
    #[command(hide = true)]
    Fim {
        prompt: String,
        #[arg(long)]
        suffix: String,
        #[arg(long = "stop")]
        stop: Vec<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        max_tokens: Option<u32>,
    },
}

#[cfg(not(test))]
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        return sigil_tui::launcher::run_tui(cli.config);
    };
    let cwd = env::current_dir()?;
    let config_path = preferred_config_path(cli.config.as_deref(), &cwd)?;
    match command {
        Commands::Run { prompt } => run_command(&config_path, &cwd, prompt).await,
        Commands::Doctor => doctor_command(&config_path, &cwd),
        Commands::Prefix {
            prompt,
            assistant_prefix,
            stop,
            model,
        } => prefix_command(&config_path, prompt, assistant_prefix, stop, model).await,
        Commands::Fim {
            prompt,
            suffix,
            stop,
            model,
            max_tokens,
        } => fim_command(&config_path, prompt, suffix, stop, model, max_tokens).await,
    }
}

fn doctor_command(config_path: &Path, launch_cwd: &Path) -> Result<()> {
    let report = build_doctor_report(config_path, launch_cwd);
    print!("{}", render_doctor_report(&report));
    Ok(())
}

fn render_doctor_report(report: &DoctorReport) -> String {
    let mut output = String::from("Sigil doctor\n");
    for check in &report.checks {
        output.push_str(&format!(
            "[{}] {} - {}\n",
            check.status.as_str(),
            check.name,
            check.message
        ));
        if let Some(remediation) = check.remediation.as_deref() {
            output.push_str(&format!("    fix: {remediation}\n"));
        }
    }
    output.push_str(&format!("summary: {}\n", report.overall_status().as_str()));
    output
}

async fn run_command(config_path: &Path, launch_cwd: &Path, prompt: String) -> Result<()> {
    let root_config = RootConfig::load(config_path)?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);

    let provider = sigil_runtime::build_provider(&root_config)?;
    let registry = sigil_runtime::build_tool_registry(
        &root_config,
        &provider.capabilities(),
        workspace_root.clone(),
    )
    .await?;
    let agent = Agent::new(provider, registry);

    let session_store = JsonlSessionStore::new(default_session_path(
        &workspace_root,
        &root_config.session.log_dir,
    ))?;
    let mut session = Session::load_from_store(
        root_config.agent.provider.clone(),
        root_config.agent.model.clone(),
        session_store,
    )?;
    let mut handler = StdoutEventHandler;
    let result = agent
        .run(
            &mut session,
            prompt,
            sigil_runtime::build_run_options(
                &root_config,
                workspace_root,
                InteractionMode::Headless,
            ),
            &mut handler,
        )
        .await?;
    if !result.final_text.is_empty() {
        println!();
    }
    if let Some(path) = session.store_path() {
        eprintln!("session log: {}", path.display());
    }
    Ok(())
}

async fn prefix_command(
    config_path: &Path,
    prompt: String,
    assistant_prefix: String,
    stop: Vec<String>,
    model: Option<String>,
) -> Result<()> {
    let root_config = RootConfig::load(config_path)?;
    let provider = DeepSeekProvider::new(sigil_runtime::load_deepseek_config(&root_config)?)?;
    let mut stream = provider
        .stream_prefix_completion(DeepSeekPrefixCompletionRequest {
            model,
            prompt,
            assistant_prefix,
            stop,
            reasoning_effort: None,
            traffic_partition_key: Some("local-user".to_owned()),
        })
        .await?;
    drain_provider_stream(&mut stream).await
}

async fn fim_command(
    config_path: &Path,
    prompt: String,
    suffix: String,
    stop: Vec<String>,
    model: Option<String>,
    max_tokens: Option<u32>,
) -> Result<()> {
    let root_config = RootConfig::load(config_path)?;
    let provider = DeepSeekProvider::new(sigil_runtime::load_deepseek_config(&root_config)?)?;
    let mut stream = provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model,
            prompt,
            suffix,
            max_tokens,
            stop,
        })
        .await?;
    drain_provider_stream(&mut stream).await
}

fn default_session_path(workspace_root: &Path, configured_log_dir: &str) -> PathBuf {
    workspace_root
        .join(configured_log_dir)
        .join(format!("session-{}.jsonl", uuid::Uuid::new_v4()))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RenderedOutput {
    stdout: String,
    stderr: String,
    stop: bool,
}

fn render_provider_chunk(chunk: ProviderChunk) -> RenderedOutput {
    match chunk {
        ProviderChunk::TextDelta(delta) => RenderedOutput {
            stdout: delta,
            ..RenderedOutput::default()
        },
        ProviderChunk::ReasoningDelta(delta) | ProviderChunk::ReasoningSummaryDelta(delta) => {
            RenderedOutput {
                stderr: format!("[reasoning] {delta}"),
                ..RenderedOutput::default()
            }
        }
        ProviderChunk::Usage(usage) => usage
            .system_fingerprint
            .map(|fingerprint| RenderedOutput {
                stderr: format!(
                    "\n[usage] prompt={} completion={} fingerprint={fingerprint}\n",
                    usage.prompt_tokens, usage.completion_tokens
                ),
                ..RenderedOutput::default()
            })
            .unwrap_or_default(),
        ProviderChunk::Done => RenderedOutput {
            stop: true,
            ..RenderedOutput::default()
        },
        _ => RenderedOutput::default(),
    }
}

fn render_run_event(event: RunEvent) -> RenderedOutput {
    match event {
        RunEvent::TextDelta(delta) => RenderedOutput {
            stdout: delta,
            ..RenderedOutput::default()
        },
        RunEvent::ReasoningDelta(delta) => RenderedOutput {
            stderr: format!("[reasoning] {delta}"),
            ..RenderedOutput::default()
        },
        RunEvent::ToolCallStarted(call) => RenderedOutput {
            stderr: format!("\n[tool:start] {} ({})\n", call.name, call.id),
            ..RenderedOutput::default()
        },
        RunEvent::ToolCallArgsDelta { id, delta } => RenderedOutput {
            stderr: format!("[tool:args:{id}] {delta}\n"),
            ..RenderedOutput::default()
        },
        RunEvent::ToolCallCompleted(call) => RenderedOutput {
            stderr: format!("[tool:complete] {} ({})\n", call.name, call.id),
            ..RenderedOutput::default()
        },
        RunEvent::ToolApprovalRequested {
            call,
            spec,
            subjects,
            preview,
        } => {
            let mut stderr = format!(
                "[tool:approval] {} ({}) {} {} subjects={}\n",
                call.name,
                call.id,
                spec.category.as_str(),
                spec.access.as_str(),
                subjects
                    .iter()
                    .map(|subject| subject.normalized.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            if let Some(preview) = preview {
                stderr.push_str(&format!("[tool:preview] {}\n", preview.summary));
            }
            RenderedOutput {
                stderr,
                ..RenderedOutput::default()
            }
        }
        RunEvent::ToolApprovalResolved {
            call_id,
            approved,
            reason,
        } => RenderedOutput {
            stderr: format!(
                "[tool:approval:{call_id}] {}{}\n",
                if approved { "approved" } else { "denied" },
                reason
                    .map(|value| format!(" ({value})"))
                    .unwrap_or_default()
            ),
            ..RenderedOutput::default()
        },
        RunEvent::ToolResult(result) => RenderedOutput {
            stderr: format!(
                "[tool:result] {} error={} {}\n",
                result.tool_name,
                result.is_error(),
                result.content
            ),
            ..RenderedOutput::default()
        },
        RunEvent::Usage(usage) => usage
            .system_fingerprint
            .map(|fingerprint| RenderedOutput {
                stderr: format!(
                    "\n[usage] prompt={} completion={} fingerprint={fingerprint}\n",
                    usage.prompt_tokens, usage.completion_tokens
                ),
                ..RenderedOutput::default()
            })
            .unwrap_or_default(),
        RunEvent::Notice(note) => RenderedOutput {
            stderr: format!("[notice] {note}\n"),
            ..RenderedOutput::default()
        },
        RunEvent::ContinuationState(_) | RunEvent::Control(_) | RunEvent::AssistantMessage(_) => {
            RenderedOutput::default()
        }
    }
}

fn emit_rendered_output(output: RenderedOutput) {
    if !output.stdout.is_empty() {
        print!("{}", output.stdout);
    }
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }
}

async fn drain_provider_stream(
    stream: &mut std::pin::Pin<Box<dyn futures::Stream<Item = Result<ProviderChunk>> + Send>>,
) -> Result<()> {
    while let Some(chunk) = stream.next().await {
        let output = render_provider_chunk(chunk?);
        let stop = output.stop;
        emit_rendered_output(output);
        if stop {
            break;
        }
    }
    println!();
    Ok(())
}

#[derive(Default)]
struct StdoutEventHandler;

impl EventHandler for StdoutEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        emit_rendered_output(render_run_event(event));
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/main_tests.rs"]
mod tests;
