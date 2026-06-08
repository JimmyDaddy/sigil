use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use termquill_kernel::{
    Agent, AgentRunOptions, EventHandler, InteractionMode, JsonlSessionStore, ProviderChunk,
    RootConfig, RunEvent, Session, preferred_config_path, resolve_workspace_root,
};
use termquill_provider_deepseek::{
    DeepSeekFimCompletionRequest, DeepSeekPrefixCompletionRequest, DeepSeekProvider,
    DeepSeekProviderConfig,
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
    let mut registry = termquill_kernel::ToolRegistry::new();
    termquill_tools_builtin::register_builtin_tools(&mut registry);
    termquill_mcp::register_mcp_tools(&mut registry, &root_config.mcp_servers).await?;

    let provider_config = load_deepseek_config(&root_config)?;
    let provider = DeepSeekProvider::new(provider_config)?;
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
            build_run_options(&root_config, workspace_root),
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
    let provider = DeepSeekProvider::new(load_deepseek_config(&root_config)?)?;
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
    let provider = DeepSeekProvider::new(load_deepseek_config(&root_config)?)?;
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

fn build_run_options(root_config: &RootConfig, workspace_root: PathBuf) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root,
        max_turns: root_config.agent.max_turns,
        tool_timeout_secs: root_config.agent.tool_timeout_secs,
        reasoning_effort: Some(termquill_kernel::ReasoningEffort::Max),
        traffic_partition_key: Some("local-user".to_owned()),
        interaction_mode: InteractionMode::Headless,
        permission_config: root_config.permission.clone(),
        memory_config: root_config.memory.clone(),
        compaction_config: root_config.compaction.clone(),
    }
}

fn default_session_path(workspace_root: &Path, configured_log_dir: &str) -> PathBuf {
    workspace_root
        .join(configured_log_dir)
        .join(format!("session-{}.jsonl", uuid::Uuid::new_v4()))
}

fn load_deepseek_config(root_config: &RootConfig) -> Result<DeepSeekProviderConfig> {
    let provider_config_value = root_config
        .providers
        .get("deepseek")
        .cloned()
        .ok_or_else(|| anyhow!("missing [providers.deepseek] in termquill.toml"))?;
    serde_json::from_value(provider_config_value).context("invalid deepseek provider config")
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
                preview,
            } => {
                eprintln!(
                    "[tool:approval] {} ({}) read_only={}",
                    call.name, call.id, spec.read_only
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
                    result.tool_name, result.is_error, result.content
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
mod tests {
    use std::collections::BTreeMap;

    use anyhow::Result;
    use serde_json::json;
    use termquill_kernel::{AgentConfig, RootConfig, SessionConfig, WorkspaceConfig};

    use super::{
        build_run_options, default_session_path, load_deepseek_config, resolve_workspace_root,
    };

    fn test_root_config() -> RootConfig {
        RootConfig {
            workspace: WorkspaceConfig {
                root: ".".to_owned(),
            },
            session: SessionConfig {
                log_dir: ".termquill/sessions".to_owned(),
            },
            agent: AgentConfig {
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-flash".to_owned(),
                max_turns: 8,
                tool_timeout_secs: 30,
            },
            permission: termquill_kernel::PermissionConfig::default(),
            memory: termquill_kernel::MemoryConfig { enabled: true },
            compaction: termquill_kernel::CompactionConfig::default(),
            providers: BTreeMap::new(),
            mcp_servers: Vec::new(),
        }
    }

    #[test]
    fn build_run_options_carries_agent_limits_and_local_partition() {
        let workspace_root = std::env::temp_dir().join("termquill-cli-test");
        let options = build_run_options(
            &RootConfig {
                workspace: WorkspaceConfig {
                    root: ".".to_owned(),
                },
                session: SessionConfig {
                    log_dir: ".termquill/sessions".to_owned(),
                },
                agent: AgentConfig {
                    provider: "deepseek".to_owned(),
                    model: "deepseek-v4-flash".to_owned(),
                    max_turns: 12,
                    tool_timeout_secs: 45,
                },
                permission: termquill_kernel::PermissionConfig::default(),
                memory: termquill_kernel::MemoryConfig { enabled: true },
                compaction: termquill_kernel::CompactionConfig::default(),
                providers: BTreeMap::new(),
                mcp_servers: Vec::new(),
            },
            workspace_root.clone(),
        );

        assert_eq!(options.workspace_root, workspace_root);
        assert_eq!(options.max_turns, 12);
        assert_eq!(options.tool_timeout_secs, 45);
        assert_eq!(
            options.reasoning_effort,
            Some(termquill_kernel::ReasoningEffort::Max)
        );
        assert_eq!(options.traffic_partition_key.as_deref(), Some("local-user"));
        assert_eq!(
            options.interaction_mode,
            termquill_kernel::InteractionMode::Headless
        );
    }

    #[test]
    fn resolve_workspace_root_uses_config_parent() -> Result<()> {
        let config_path = std::env::temp_dir()
            .join("termquill-cli-config-parent")
            .join("termquill.toml");
        let launch_cwd = std::env::temp_dir().join("termquill-cli-launch");
        let resolved = resolve_workspace_root(&config_path, &launch_cwd, "workspace/project");

        assert_eq!(
            resolved,
            config_path
                .parent()
                .expect("config path should have a parent")
                .join("workspace/project")
        );
        Ok(())
    }

    #[test]
    fn resolve_workspace_root_uses_launch_cwd_for_default_dot() {
        let config_path = std::env::temp_dir()
            .join("termquill-cli-config-parent")
            .join("termquill.toml");
        let launch_cwd = std::env::temp_dir().join("termquill-cli-launch");

        let resolved = resolve_workspace_root(&config_path, &launch_cwd, ".");

        assert_eq!(resolved, launch_cwd);
    }

    #[test]
    fn default_session_path_uses_configured_log_dir_and_jsonl_suffix() {
        let workspace_root = std::env::temp_dir().join("termquill-cli-workspace");
        let session_path = default_session_path(&workspace_root, ".termquill/sessions");

        assert!(session_path.starts_with(workspace_root.join(".termquill/sessions")));
        assert_eq!(
            session_path.extension().and_then(|ext| ext.to_str()),
            Some("jsonl")
        );
        assert!(
            session_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("session-"))
        );
    }

    #[test]
    fn load_deepseek_config_reads_provider_block() -> Result<()> {
        let mut root_config = test_root_config();
        root_config.providers.insert(
            "deepseek".to_owned(),
            json!({
                "base_url": "https://example.com",
                "beta_base_url": "https://example.com/beta",
                "anthropic_base_url": "https://example.com/anthropic",
                "model": "deepseek-v4-pro",
                "fim_model": "deepseek-v4-fim",
                "api_key": "test-key",
                "strict_tools_mode": "always",
                "request_timeout_secs": 15
            }),
        );

        let config = load_deepseek_config(&root_config)?;
        assert_eq!(config.base_url, "https://example.com");
        assert_eq!(config.beta_base_url, "https://example.com/beta");
        assert_eq!(config.anthropic_base_url, "https://example.com/anthropic");
        assert_eq!(config.model, "deepseek-v4-pro");
        assert_eq!(config.fim_model, "deepseek-v4-fim");
        assert_eq!(config.api_key.as_deref(), Some("test-key"));
        assert_eq!(
            config.strict_tools_mode,
            termquill_provider_deepseek::StrictToolsMode::Always
        );
        assert_eq!(config.request_timeout_secs, 15);
        Ok(())
    }

    #[test]
    fn load_deepseek_config_errors_when_provider_block_is_missing() {
        let root_config = test_root_config();
        let error =
            load_deepseek_config(&root_config).expect_err("expected missing provider error");

        assert!(
            error
                .to_string()
                .contains("missing [providers.deepseek] in termquill.toml")
        );
    }
}
