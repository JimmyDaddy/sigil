use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

#[cfg(not(test))]
use std::env;

use anyhow::Result;
use clap::{Parser, Subcommand};
use futures::StreamExt;
use sigil_http::{DEFAULT_HTTP_TOKEN_ENV, HttpAuthConfig, HttpServerConfig};
#[cfg(not(test))]
use sigil_kernel::preferred_config_path;
use sigil_kernel::{
    Agent, EventHandler, InteractionMode, JsonlSessionStore, ProviderChunk, RootConfig, RunEvent,
    Session, UsageStats, resolve_workspace_root,
};
use sigil_runtime::doctor::{DoctorReport, DoctorReportOptions, build_doctor_report_with_options};
use sigil_runtime::{
    DeepSeekFimDebugRequest, DeepSeekPrefixDebugRequest, stream_deepseek_fim_debug,
    stream_deepseek_prefix_debug,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BuildInfo {
    version: &'static str,
    git_hash: &'static str,
    target: &'static str,
    profile: &'static str,
}

impl BuildInfo {
    fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            git_hash: env!("SIGIL_BUILD_GIT_HASH"),
            target: env!("SIGIL_BUILD_TARGET"),
            profile: env!("SIGIL_BUILD_PROFILE"),
        }
    }
}

#[derive(Parser)]
#[command(name = "sigil")]
#[command(about = "TUI-first shell for Sigil")]
#[command(disable_version_flag = true)]
struct Cli {
    #[arg(long = "version", action = clap::ArgAction::SetTrue)]
    show_version: bool,
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
    Resume {
        session: Option<String>,
    },
    Doctor,
    Serve {
        #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
        host: IpAddr,
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long = "token-env", default_value = DEFAULT_HTTP_TOKEN_ENV)]
        token_env: String,
        #[arg(long = "no-token", action = clap::ArgAction::SetTrue)]
        no_token: bool,
    },
    // Hidden provider-specific developer diagnostics. Keep ordinary users on the
    // TUI, `run`, `doctor`, or explicit provider configuration surfaces.
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
    if cli.show_version {
        print!("{}", render_version(BuildInfo::current()));
        return Ok(());
    }
    let Some(command) = cli.command else {
        return sigil_tui::launcher::run_tui(cli.config);
    };
    let cwd = env::current_dir()?;
    let config_path = preferred_config_path(cli.config.as_deref(), &cwd)?;
    match command {
        Commands::Run { prompt } => run_command(&config_path, &cwd, prompt).await,
        Commands::Resume { session } => sigil_tui::launcher::run_tui_resume(cli.config, session),
        Commands::Doctor => doctor_command(&config_path, &cwd),
        Commands::Serve {
            host,
            port,
            token_env,
            no_token,
        } => {
            let token = if no_token {
                None
            } else {
                env::var(&token_env).ok()
            };
            serve_command(
                ServeOptions {
                    host,
                    port,
                    token_env,
                    no_token,
                },
                token.as_deref(),
            )
        }
        Commands::Prefix {
            prompt,
            assistant_prefix,
            stop,
            model,
        } => prefix_command(&config_path, &cwd, prompt, assistant_prefix, stop, model).await,
        Commands::Fim {
            prompt,
            suffix,
            stop,
            model,
            max_tokens,
        } => fim_command(&config_path, prompt, suffix, stop, model, max_tokens).await,
    }
}

fn render_version(info: BuildInfo) -> String {
    format!(
        "sigil {}\ncommit: {}\ntarget: {}\nprofile: {}\n",
        info.version, info.git_hash, info.target, info.profile
    )
}

fn doctor_command(config_path: &Path, launch_cwd: &Path) -> Result<()> {
    print!("{}", render_cli_doctor_report(config_path, launch_cwd));
    Ok(())
}

fn render_cli_doctor_report(config_path: &Path, launch_cwd: &Path) -> String {
    let report = build_cli_doctor_report(config_path, launch_cwd);
    render_doctor_report(&report)
}

fn build_cli_doctor_report(config_path: &Path, launch_cwd: &Path) -> DoctorReport {
    build_doctor_report_with_options(
        config_path,
        launch_cwd,
        DoctorReportOptions {
            appearance_checks: Some(&sigil_tui::appearance_diagnostics::appearance_doctor_checks),
            ..DoctorReportOptions::default()
        },
    )
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServeOptions {
    host: IpAddr,
    port: u16,
    token_env: String,
    no_token: bool,
}

impl ServeOptions {
    fn http_config(&self) -> HttpServerConfig {
        HttpServerConfig {
            bind_host: self.host,
            port: self.port,
            auth: HttpAuthConfig {
                require_token: !self.no_token,
                token_env: self.token_env.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServeStartupPlan {
    bind_addr: SocketAddr,
    token_required: bool,
    token_env: Option<String>,
}

fn serve_command(options: ServeOptions, token: Option<&str>) -> Result<()> {
    let plan = build_serve_startup_plan(options, token)?;
    print!("{}", render_serve_startup_plan(&plan));
    Ok(())
}

fn build_serve_startup_plan(
    options: ServeOptions,
    token: Option<&str>,
) -> Result<ServeStartupPlan> {
    let config = options.http_config();
    config.validate()?;
    let validator = config.auth.validator_from_token(token)?;
    Ok(ServeStartupPlan {
        bind_addr: config.bind_addr(),
        token_required: validator.token_required(),
        token_env: if config.auth.require_token {
            Some(config.auth.token_env)
        } else {
            None
        },
    })
}

fn render_serve_startup_plan(plan: &ServeStartupPlan) -> String {
    let auth = if plan.token_required {
        let token_env = plan.token_env.as_deref().unwrap_or(DEFAULT_HTTP_TOKEN_ENV);
        format!("bearer token from {token_env}")
    } else {
        "disabled".to_owned()
    };
    format!(
        "Sigil HTTP/SSE adapter\nbind: {}\nauth: {}\nstatus: HTTP routing is not implemented yet; no listener started\n",
        plan.bind_addr, auth
    )
}

async fn run_command(config_path: &Path, launch_cwd: &Path, prompt: String) -> Result<()> {
    let root_config = RootConfig::load(config_path)?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let sigil_paths = sigil_runtime::resolve_sigil_paths(
        &root_config.storage,
        &root_config.session,
        &workspace_root,
    );

    let provider = sigil_runtime::build_provider(&root_config)?;
    let registry = sigil_runtime::build_tool_registry(
        &root_config,
        &provider.capabilities(),
        workspace_root.clone(),
    )
    .await?;
    let agent = Agent::new(provider, registry);

    let session_store = JsonlSessionStore::new(default_session_path(&sigil_paths.session_log_dir))?;
    let mut session = Session::load_from_store(
        root_config.agent.provider.clone(),
        root_config.agent.model.clone(),
        session_store,
    )?;
    let mut handler = StdoutEventHandler;
    let result = agent
        .run_with_input(
            &mut session,
            run_input_with_repo_context(&workspace_root, prompt),
            sigil_runtime::build_run_options(
                &root_config,
                workspace_root,
                InteractionMode::Headless,
            ),
            &mut handler,
        )
        .await?
        .result;
    if !result.final_text.is_empty() {
        println!();
    }
    if let Some(path) = session.store_path() {
        eprintln!("session log: {}", path.display());
    }
    Ok(())
}

fn run_input_with_repo_context(
    workspace_root: &Path,
    prompt: String,
) -> sigil_kernel::AgentRunInput {
    let runtime_context =
        sigil_runtime::context_candidates_from_safe_sources(workspace_root, &prompt, None)
            .unwrap_or_default();
    sigil_kernel::AgentRunInput::user(prompt).with_runtime_context(runtime_context)
}

async fn prefix_command(
    config_path: &Path,
    launch_cwd: &Path,
    prompt: String,
    assistant_prefix: String,
    stop: Vec<String>,
    model: Option<String>,
) -> Result<()> {
    let root_config = RootConfig::load(config_path)?;
    let mut stream = stream_deepseek_prefix_debug(
        &root_config,
        config_path,
        launch_cwd,
        DeepSeekPrefixDebugRequest {
            prompt,
            assistant_prefix,
            stop,
            model,
        },
    )
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
    let mut stream = stream_deepseek_fim_debug(
        &root_config,
        DeepSeekFimDebugRequest {
            prompt,
            suffix,
            max_tokens,
            stop,
            model,
        },
    )
    .await?;
    drain_provider_stream(&mut stream).await
}

fn default_session_path(session_log_dir: &Path) -> PathBuf {
    session_log_dir.join(format!("session-{}.jsonl", uuid::Uuid::new_v4()))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RenderedOutput {
    stdout: String,
    stderr: String,
    stop: bool,
}

enum StreamRenderEvent {
    TextDelta(String),
    ReasoningDelta(String),
    Usage(UsageStats),
    Done,
}

fn render_stream_event(event: StreamRenderEvent) -> RenderedOutput {
    match event {
        StreamRenderEvent::TextDelta(delta) => RenderedOutput {
            stdout: delta,
            ..RenderedOutput::default()
        },
        StreamRenderEvent::ReasoningDelta(delta) => RenderedOutput {
            stderr: format!("[reasoning] {delta}"),
            ..RenderedOutput::default()
        },
        StreamRenderEvent::Usage(usage) => usage
            .system_fingerprint
            .map(|fingerprint| RenderedOutput {
                stderr: format!(
                    "\n[usage] prompt={} completion={} fingerprint={fingerprint}\n",
                    usage.prompt_tokens, usage.completion_tokens
                ),
                ..RenderedOutput::default()
            })
            .unwrap_or_default(),
        StreamRenderEvent::Done => RenderedOutput {
            stop: true,
            ..RenderedOutput::default()
        },
    }
}

fn render_provider_chunk(chunk: ProviderChunk) -> RenderedOutput {
    match chunk {
        ProviderChunk::TextDelta(delta) => render_stream_event(StreamRenderEvent::TextDelta(delta)),
        ProviderChunk::ReasoningDelta(delta) | ProviderChunk::ReasoningSummaryDelta(delta) => {
            render_stream_event(StreamRenderEvent::ReasoningDelta(delta))
        }
        ProviderChunk::Usage(usage) => render_stream_event(StreamRenderEvent::Usage(usage)),
        ProviderChunk::Done => render_stream_event(StreamRenderEvent::Done),
        _ => RenderedOutput::default(),
    }
}

fn render_run_event(event: RunEvent) -> RenderedOutput {
    match event {
        RunEvent::TextDelta(delta) => render_stream_event(StreamRenderEvent::TextDelta(delta)),
        RunEvent::ReasoningDelta(delta) => {
            render_stream_event(StreamRenderEvent::ReasoningDelta(delta))
        }
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
            ..
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
        RunEvent::ToolProgress(progress) => {
            let mut stderr = format!(
                "[tool:progress] {} ({}) {}\n",
                progress.tool_name, progress.call_id, progress.status
            );
            if let Some(message) = progress.message {
                stderr.push_str(&format!("[tool:progress:message] {message}\n"));
            }
            if let Some(output_preview) = progress.output_preview {
                stderr.push_str(&format!("[tool:progress:preview] {output_preview}\n"));
            }
            RenderedOutput {
                stderr,
                ..RenderedOutput::default()
            }
        }
        RunEvent::Usage(usage) => render_stream_event(StreamRenderEvent::Usage(usage)),
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
