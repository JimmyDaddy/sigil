use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::Result;
use clap::{Parser, Subcommand};
use futures::StreamExt;
use termquill_kernel::{
    Agent, EventHandler, InteractionMode, JsonlSessionStore, ProviderChunk, RootConfig, RunEvent,
    Session, preferred_config_path, resolve_workspace_root,
};
use termquill_provider_deepseek::{
    DeepSeekFimCompletionRequest, DeepSeekPrefixCompletionRequest, DeepSeekProvider,
};

#[derive(Parser)]
#[command(name = "termquill")]
#[command(about = "DeepSeek-first coding agent prototype")]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run {
        prompt: String,
    },
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let cwd = env::current_dir()?;
    let config_path = preferred_config_path(cli.config.as_deref(), &cwd)?;
    match cli.command {
        Commands::Run { prompt } => run_command(&config_path, &cwd, prompt).await,
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

async fn run_command(config_path: &Path, launch_cwd: &Path, prompt: String) -> Result<()> {
    let root_config = RootConfig::load(config_path)?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);

    let provider = termquill_runtime::build_provider(&root_config)?;
    let registry = termquill_runtime::build_tool_registry(
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
            termquill_runtime::build_run_options(
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
    let provider = DeepSeekProvider::new(termquill_runtime::load_deepseek_config(&root_config)?)?;
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
    let provider = DeepSeekProvider::new(termquill_runtime::load_deepseek_config(&root_config)?)?;
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

async fn drain_provider_stream(
    stream: &mut std::pin::Pin<Box<dyn futures::Stream<Item = Result<ProviderChunk>> + Send>>,
) -> Result<()> {
    while let Some(chunk) = stream.next().await {
        match chunk? {
            ProviderChunk::TextDelta(delta) => print!("{delta}"),
            ProviderChunk::ReasoningDelta(delta) | ProviderChunk::ReasoningSummaryDelta(delta) => {
                eprint!("[reasoning] {delta}")
            }
            ProviderChunk::Usage(usage) => {
                if let Some(fingerprint) = usage.system_fingerprint {
                    eprintln!(
                        "\n[usage] prompt={} completion={} fingerprint={}",
                        usage.prompt_tokens, usage.completion_tokens, fingerprint
                    );
                }
            }
            ProviderChunk::Done => break,
            _ => {}
        }
    }
    println!();
    Ok(())
}

#[derive(Default)]
struct StdoutEventHandler;

impl EventHandler for StdoutEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        match event {
            RunEvent::TextDelta(delta) => {
                print!("{delta}");
            }
            RunEvent::ReasoningDelta(delta) => {
                eprint!("[reasoning] {delta}");
            }
            RunEvent::ToolCallStarted(call) => {
                eprintln!("\n[tool:start] {} ({})", call.name, call.id);
            }
            RunEvent::ToolCallArgsDelta { id, delta } => {
                eprintln!("[tool:args:{id}] {delta}");
            }
            RunEvent::ToolCallCompleted(call) => {
                eprintln!("[tool:complete] {} ({})", call.name, call.id);
            }
            RunEvent::ToolApprovalRequested {
                call,
                spec,
                subjects,
                preview,
            } => {
                eprintln!(
                    "[tool:approval] {} ({}) {} {} subjects={}",
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
                    eprintln!("[tool:preview] {}", preview.summary);
                }
            }
            RunEvent::ToolApprovalResolved {
                call_id,
                approved,
                reason,
            } => {
                eprintln!(
                    "[tool:approval:{}] {}{}",
                    call_id,
                    if approved { "approved" } else { "denied" },
                    reason
                        .map(|value| format!(" ({value})"))
                        .unwrap_or_default()
                );
            }
            RunEvent::ToolResult(result) => {
                eprintln!(
                    "[tool:result] {} error={} {}",
                    result.tool_name,
                    result.is_error(),
                    result.content
                );
            }
            RunEvent::Usage(usage) => {
                if let Some(fingerprint) = usage.system_fingerprint {
                    eprintln!(
                        "\n[usage] prompt={} completion={} fingerprint={}",
                        usage.prompt_tokens, usage.completion_tokens, fingerprint
                    );
                }
            }
            RunEvent::Notice(note) => eprintln!("[notice] {note}"),
            RunEvent::ContinuationState(_)
            | RunEvent::Control(_)
            | RunEvent::AssistantMessage(_) => {}
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/main_tests.rs"]
mod tests;
