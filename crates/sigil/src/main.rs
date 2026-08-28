use std::{
    env,
    future::Future,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    thread::JoinHandle,
};

pub mod egress_disclosure;
#[cfg_attr(test, allow(dead_code))]
mod intent_cli;
mod mcp_cli;

#[cfg(not(test))]
use std::io;
#[cfg(not(test))]
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use futures::StreamExt;
use sigil_http::{DEFAULT_HTTP_TOKEN_ENV, HttpAuthConfig, HttpServerConfig, HttpServerInfo};
#[cfg(not(test))]
use sigil_http::{
    HttpDurableCommandStore, HttpDurableEgressDisclosureJournal, HttpDurableProtocolJournal,
    HttpLiveEventBus, HttpLocalServer, HttpProductionRunDriver, HttpProductionRunDriverOptions,
    HttpSupportContext,
};
#[cfg(not(test))]
use sigil_kernel::preferred_config_path;
use sigil_kernel::{
    AutoApproveHandler, EventHandler, ProviderChunk, PublicRunEvent, PublicRunEventKind,
    RootConfig, RunEvent, UsageStats, resolve_workspace_root,
};
use sigil_runtime::doctor::{DoctorReport, DoctorReportOptions, build_doctor_report_with_options};
use sigil_runtime::{
    DeepSeekFimDebugRequest, DeepSeekPrefixDebugRequest,
    application_run::{
        ApplicationRunEventHandler, ApplicationRunPrepareError, ApplicationRunPrepareErrorClass,
        ApplicationRunRequest, ApplicationRunServices, ApplicationRunTerminalStatus,
        prepare_application_run,
    },
    machine_protocol::{
        MachineError, MachineErrorCode, MachineExitCode, MachineRecord, MachineRunResult,
        MachineRunStatus,
    },
    resolve_sigil_paths, secret_redactor_for_root_config, stream_deepseek_fim_debug,
    stream_deepseek_prefix_debug,
    support::{
        DoctorSupportProjectionContext, DoctorSupportReportV1, SupportBuildInfo,
        SupportEnvironmentV1, SupportPathKind, SupportPathRedaction,
        project_doctor_support_report_v1,
    },
};
use sigil_runtime::{
    LocalSessionLifecycleService, SessionCatalogProjectionError, SessionCatalogProjectionService,
    SigilPaths,
};

const HTTP_SERVER_STATE_DIR: &str = "http-server-v4";
#[cfg(not(test))]
const HTTP_PROTOCOL_JOURNAL_FILE: &str = "protocol-events.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BuildInfo {
    version: &'static str,
    git_hash: &'static str,
    target: &'static str,
    profile: &'static str,
    distribution: &'static str,
}

impl BuildInfo {
    fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            git_hash: env!("SIGIL_BUILD_GIT_HASH"),
            target: env!("SIGIL_BUILD_TARGET"),
            profile: env!("SIGIL_BUILD_PROFILE"),
            distribution: env!("SIGIL_BUILD_DISTRIBUTION"),
        }
    }

    fn update_metadata(self) -> sigil_updater::BuildMetadata {
        sigil_updater::BuildMetadata::new(
            self.version,
            self.target,
            self.profile,
            self.distribution,
        )
    }
}

impl From<BuildInfo> for SupportBuildInfo {
    fn from(value: BuildInfo) -> Self {
        Self::new(value.version, value.git_hash, value.target, value.profile)
    }
}

#[derive(Parser)]
#[command(name = "sigil")]
#[command(about = "Terminal workspace for the Sigil coding agent")]
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
        #[arg(long, value_enum, default_value = "text")]
        output: RunOutput,
        #[arg(long, requires = "model")]
        connection: Option<String>,
        #[arg(long, requires = "connection")]
        model: Option<String>,
        #[arg(long, value_name = "JSONL_PATH")]
        session: Option<PathBuf>,
        #[arg(long, value_name = "BINDING", requires = "session")]
        route_recovery_binding: Option<String>,
    },
    Resume {
        session: Option<String>,
    },
    Doctor {
        #[arg(long, value_enum, default_value = "text")]
        output: DoctorOutput,
        #[command(subcommand)]
        command: Option<DoctorCommand>,
    },
    /// Emit typed JSON Intent Stack automation records for one exact durable session.
    Intent {
        #[arg(long, help = "Exact durable session ID in the current workspace")]
        session: String,
        #[command(subcommand)]
        command: intent_cli::IntentCommand,
    },
    /// Manage MCP servers in the selected Sigil user configuration.
    Mcp {
        #[command(subcommand)]
        command: mcp_cli::McpCommand,
    },
    Tokenizer {
        #[command(subcommand)]
        command: TokenizerCommand,
    },
    /// Check for or install a Sigil update.
    Update {
        #[command(subcommand)]
        command: UpdateCommand,
    },
    /// Apply one typed plan decision (Run | Save | Revise | Reject) to a durable session that is
    /// awaiting a plan decision. The decision is hash-bound and never auto-accepts execution.
    PlanDecision {
        #[arg(long, value_name = "JSONL_PATH")]
        session: PathBuf,
        #[arg(long)]
        plan_id: String,
        #[arg(long)]
        plan_hash: String,
        #[arg(long, value_enum)]
        action: PlanDecisionAction,
    },
    Serve {
        #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
        host: IpAddr,
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long = "token-env", default_value = DEFAULT_HTTP_TOKEN_ENV)]
        token_env: String,
        #[arg(long = "no-token", action = clap::ArgAction::SetTrue)]
        no_token: bool,
        #[arg(long = "startup-output", value_enum, default_value = "text")]
        startup_output: ServeStartupOutput,
        #[arg(long = "shutdown-on-stdin-close", action = clap::ArgAction::SetTrue)]
        shutdown_on_stdin_close: bool,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum PlanDecisionAction {
    Run,
    Save,
    Revise,
    Reject,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum RunOutput {
    #[default]
    Text,
    Json,
    Jsonl,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum DoctorOutput {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
enum DoctorCommand {
    /// Recover a failed authority journal into fresh storage roots configured in sigil.toml.
    ///
    /// The configured state, cache, and scratch roots must already be distinct, empty,
    /// owner-only directories. The command prints an operation-bound challenge and changes
    /// authority epoch only after that exact challenge is typed back.
    RecoverAuthority,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum ServeStartupOutput {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum UpdateOutput {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum UpdateChannelArg {
    #[default]
    Current,
    Stable,
    Beta,
}

impl From<UpdateChannelArg> for sigil_updater::UpdateChannel {
    fn from(value: UpdateChannelArg) -> Self {
        match value {
            UpdateChannelArg::Current => Self::Current,
            UpdateChannelArg::Stable => Self::Stable,
            UpdateChannelArg::Beta => Self::Beta,
        }
    }
}

#[derive(Subcommand)]
enum TokenizerCommand {
    /// Explicitly download and checksum-verify the public tokenizer needed for DeepSeek V4 Flash portable compaction.
    Install { profile: String },
}

#[derive(Subcommand)]
enum UpdateCommand {
    /// Check GitHub Releases without changing the current installation.
    Check {
        #[arg(long, value_enum, default_value = "current")]
        channel: UpdateChannelArg,
        #[arg(long)]
        refresh: bool,
        #[arg(long, value_enum, default_value = "text")]
        output: UpdateOutput,
    },
    /// Install an admitted standalone update or print the owning package-manager command.
    Apply {
        #[arg(long, value_enum, default_value = "current")]
        channel: UpdateChannelArg,
        #[arg(long)]
        yes: bool,
        #[arg(long, value_enum, default_value = "text")]
        output: UpdateOutput,
    },
}

#[cfg(not(test))]
fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(&cli);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: failed to start async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run_main(cli));
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn interactive_tui_requested(cli: &Cli) -> bool {
    !cli.show_version && matches!(cli.command.as_ref(), None | Some(Commands::Resume { .. }))
}

#[cfg(not(test))]
fn init_tracing(cli: &Cli) {
    if interactive_tui_requested(cli) {
        // The TUI owns the terminal byte stream. A formatting subscriber writing to stderr can
        // interleave arbitrary bytes with Ratatui's stdout paint and permanently corrupt the
        // inline viewport, so interactive runs deliberately keep tracing off the terminal.
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(io::sink)
            .init();
    } else {
        tracing_subscriber::fmt::init();
    }
}

#[cfg(not(test))]
async fn run_main(cli: Cli) -> Result<u8> {
    let build = BuildInfo::current();
    if cli.show_version {
        print!("{}", render_version(build));
        return Ok(0);
    }
    let Some(command) = cli.command else {
        sigil_tui_app::launcher::run_tui_with_build_context(
            cli.config,
            build.into(),
            build.update_metadata(),
        )?;
        return Ok(0);
    };
    let machine_output = match &command {
        Commands::Run { output, .. } if *output != RunOutput::Text => Some(*output),
        _ => None,
    };
    let intent_session_id = match &command {
        Commands::Intent { session, .. } => Some(session.as_str()),
        _ => None,
    };
    let cwd = match env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            if let Some(session_id) = intent_session_id {
                return Ok(u8::try_from(
                    intent_cli::IntentCommandExecution::bootstrap_error(
                        session_id,
                        intent_cli::IntentAutomationErrorCode::ConfigurationInvalid,
                    )
                    .write_json()
                    .as_i32(),
                )
                .expect("machine exit codes must fit in u8"));
            }
            if machine_output.is_some() {
                eprintln!("sigil run: process working directory is unavailable");
                return Ok(write_bootstrap_machine_error(
                    MachineError::new(
                        MachineErrorCode::Internal,
                        "process working directory is unavailable",
                        false,
                    ),
                    MachineExitCode::ExecutionFailed,
                ));
            }
            return Err(error.into());
        }
    };
    let config_path = match preferred_config_path(cli.config.as_deref(), &cwd) {
        Ok(path) => path,
        Err(error) => {
            if let Some(session_id) = intent_session_id {
                return Ok(u8::try_from(
                    intent_cli::IntentCommandExecution::bootstrap_error(
                        session_id,
                        intent_cli::IntentAutomationErrorCode::ConfigurationInvalid,
                    )
                    .write_json()
                    .as_i32(),
                )
                .expect("machine exit codes must fit in u8"));
            }
            if machine_output.is_some() {
                eprintln!("sigil run: application configuration path is unavailable");
                return Ok(write_bootstrap_machine_error(
                    MachineError::new(
                        MachineErrorCode::ConfigurationInvalid,
                        "application configuration path is unavailable",
                        false,
                    ),
                    MachineExitCode::InvalidInput,
                ));
            }
            return Err(error);
        }
    };
    match command {
        Commands::Run {
            prompt,
            output: RunOutput::Text,
            connection,
            model,
            session,
            route_recovery_binding,
        } => {
            run_command(
                &config_path,
                &cwd,
                prompt,
                connection.as_deref(),
                model.as_deref(),
                session.as_deref(),
                route_recovery_binding.as_deref(),
            )
            .await?
        }
        Commands::Run {
            prompt,
            output,
            connection,
            model,
            session,
            route_recovery_binding,
        } => {
            let code = run_machine_command(
                &config_path,
                &cwd,
                prompt,
                output,
                connection.as_deref(),
                model.as_deref(),
                session.as_deref(),
                route_recovery_binding.as_deref(),
            )
            .await
            .as_i32();
            return Ok(u8::try_from(code).expect("machine exit codes must fit in u8"));
        }
        Commands::Resume { session } => {
            sigil_tui_app::launcher::run_tui_resume_with_build_context(
                cli.config,
                session,
                build.into(),
                build.update_metadata(),
            )?;
        }
        Commands::Doctor { output, command } => match command {
            Some(DoctorCommand::RecoverAuthority) => {
                authority_recovery_command(&config_path, &cwd)?
            }
            None => doctor_command(&config_path, &cwd, output)?,
        },
        Commands::Intent { session, command } => {
            let exit = intent_cli::execute_intent_command(&config_path, &cwd, &session, command)
                .write_json();
            return Ok(u8::try_from(exit.as_i32()).expect("machine exit codes must fit in u8"));
        }
        Commands::Mcp { command } => {
            print!("{}", mcp_cli::execute_mcp_command(&config_path, command)?);
        }
        Commands::Tokenizer { command } => {
            tokenizer_command(&config_path, &cwd, command).await?;
        }
        Commands::Update { command } => {
            update_command(&config_path, &cwd, build, command).await?;
        }
        Commands::PlanDecision {
            session,
            plan_id,
            plan_hash,
            action,
        } => {
            println!(
                "{}",
                plan_decision_command(&config_path, &cwd, &session, &plan_id, &plan_hash, action)
                    .await?
            );
        }
        Commands::Serve {
            host,
            port,
            token_env,
            no_token,
            startup_output,
            shutdown_on_stdin_close,
        } => {
            let token = if no_token {
                None
            } else {
                env::var(&token_env).ok()
            };
            serve_command(
                &config_path,
                &cwd,
                ServeOptions {
                    host,
                    port,
                    token_env,
                    no_token,
                    startup_output,
                    shutdown_on_stdin_close,
                },
                token.as_deref(),
            )
            .await?;
        }
        Commands::Prefix {
            prompt,
            assistant_prefix,
            stop,
            model,
        } => prefix_command(&config_path, &cwd, prompt, assistant_prefix, stop, model).await?,
        Commands::Fim {
            prompt,
            suffix,
            stop,
            model,
            max_tokens,
        } => fim_command(&config_path, prompt, suffix, stop, model, max_tokens).await?,
    }
    Ok(0)
}

#[cfg(not(test))]
fn write_bootstrap_machine_error(error: MachineError, exit: MachineExitCode) -> u8 {
    let mut stdout = io::stdout();
    let actual = write_machine_terminal(&mut stdout, MachineRecord::error(error), exit).as_i32();
    u8::try_from(actual).expect("machine exit codes must fit in u8")
}

#[cfg(not(test))]
async fn tokenizer_command(
    config_path: &Path,
    launch_cwd: &Path,
    command: TokenizerCommand,
) -> Result<()> {
    let TokenizerCommand::Install { profile } = command;
    if profile != "deepseek-v4-flash" {
        anyhow::bail!("unsupported tokenizer profile {profile}; supported: deepseek-v4-flash");
    }
    let config = RootConfig::load(config_path)?;
    let workspace_root = resolve_workspace_root(config_path, launch_cwd, &config.workspace.root);
    let paths =
        sigil_runtime::resolve_sigil_paths(&config.storage, &config.session, &workspace_root);
    eprintln!(
        "network disclosure: downloading the public checksum-pinned DeepSeek V4 Flash tokenizer artifact for local portable-compaction setup"
    );
    let installed =
        sigil_runtime::install_default_deepseek_v4_flash_tokenizer(&paths.cache_root).await?;
    println!(
        "installed verified DeepSeek V4 Flash tokenizer at {}",
        installed.display()
    );
    Ok(())
}

#[cfg(not(test))]
async fn update_command(
    config_path: &Path,
    launch_cwd: &Path,
    build: BuildInfo,
    command: UpdateCommand,
) -> Result<()> {
    let paths = match RootConfig::load(config_path) {
        Ok(config) => {
            let workspace_root =
                resolve_workspace_root(config_path, launch_cwd, &config.workspace.root);
            resolve_sigil_paths(&config.storage, &config.session, &workspace_root)
        }
        Err(_) => resolve_sigil_paths(
            &sigil_kernel::StorageConfig::default(),
            &sigil_kernel::SessionConfig::default(),
            launch_cwd,
        ),
    };
    let current_exe = env::current_exe()?;
    let metadata = build.update_metadata();
    let install_source = metadata.install_source(&current_exe);

    match command {
        UpdateCommand::Check {
            channel,
            refresh,
            output,
        } => {
            let service = sigil_updater::UpdateService::github(&paths.cache_root)?;
            let outcome = service
                .check(sigil_updater::CheckOptions {
                    current_version: metadata.version,
                    target: metadata.target,
                    channel: channel.into(),
                    install_source,
                    force_refresh: refresh,
                })
                .await?;
            match output {
                UpdateOutput::Text => print!("{}", render_update_check(&outcome)),
                UpdateOutput::Json => println!("{}", serde_json::to_string_pretty(&outcome)?),
            }
        }
        UpdateCommand::Apply {
            channel,
            yes,
            output,
        } => {
            if !yes {
                anyhow::bail!("refusing to update without explicit --yes");
            }
            if matches!(
                install_source,
                sigil_updater::InstallSource::Source | sigil_updater::InstallSource::Unknown
            ) {
                anyhow::bail!(
                    "this source build cannot update itself; rebuild or reinstall with Cargo"
                );
            }
            let service = sigil_updater::UpdateService::github(&paths.cache_root)?;
            let outcome = service
                .check(sigil_updater::CheckOptions {
                    current_version: metadata.version,
                    target: metadata.target,
                    channel: channel.into(),
                    install_source,
                    force_refresh: true,
                })
                .await?;
            let applied = sigil_updater::apply_checked_update(&outcome, &current_exe).await?;
            match output {
                UpdateOutput::Text => print!("{}", render_update_apply(&applied)),
                UpdateOutput::Json => println!("{}", serde_json::to_string_pretty(&applied)?),
            }
        }
    }
    Ok(())
}

fn render_update_check(outcome: &sigil_updater::UpdateCheckOutcome) -> String {
    let mut rendered = format!(
        "Sigil update check\ncurrent: {}\nchannel: {}\ninstall source: {}\n",
        outcome.current_version,
        outcome.channel,
        install_source_label(outcome.install_source)
    );
    let Some(candidate) = outcome.candidate.as_ref() else {
        rendered.push_str("status: up to date\n");
        return rendered;
    };
    rendered.push_str(&format!("available: {}\n", candidate.version));
    if let Some(command) = outcome.managed_update_command.as_deref() {
        rendered.push_str(&format!("update command: {command}\n"));
    } else if outcome.apply_permitted() {
        rendered.push_str("status: ready to install with `sigil update apply --yes`\n");
    } else {
        let blocking_reason = candidate
            .security
            .blocking_reason
            .as_deref()
            .unwrap_or("this installation source cannot be updated in place");
        rendered.push_str(&format!("status: install blocked ({})\n", blocking_reason));
    }
    rendered
}

fn render_update_apply(outcome: &sigil_updater::UpdateApplyOutcome) -> String {
    match outcome {
        sigil_updater::UpdateApplyOutcome::Installed { version } => {
            format!("Sigil {version} installed. Restart Sigil to use the new version.\n")
        }
        sigil_updater::UpdateApplyOutcome::ManagedExternally { command } => {
            format!("This installation is managed externally. Run:\n{command}\n")
        }
    }
}

const fn install_source_label(source: sigil_updater::InstallSource) -> &'static str {
    match source {
        sigil_updater::InstallSource::StandaloneGitHubArchive => "standalone GitHub archive",
        sigil_updater::InstallSource::Npm => "npm",
        sigil_updater::InstallSource::Homebrew => "Homebrew",
        sigil_updater::InstallSource::Cargo => "Cargo",
        sigil_updater::InstallSource::Source => "source build",
        sigil_updater::InstallSource::Unknown => "unknown",
    }
}

fn render_version(info: BuildInfo) -> String {
    format!(
        "sigil {}\ncommit: {}\ntarget: {}\nprofile: {}\ndistribution: {}\n",
        info.version, info.git_hash, info.target, info.profile, info.distribution
    )
}

fn doctor_command(config_path: &Path, launch_cwd: &Path, output: DoctorOutput) -> Result<()> {
    match output {
        DoctorOutput::Text => print!("{}", render_cli_doctor_report(config_path, launch_cwd)),
        DoctorOutput::Json => println!(
            "{}",
            build_cli_doctor_support_report(config_path, launch_cwd)?.to_pretty_json()?
        ),
    }
    Ok(())
}

#[cfg(not(test))]
fn authority_recovery_command(config_path: &Path, launch_cwd: &Path) -> Result<()> {
    let summary = sigil_runtime::doctor::recover_authority_bootstrap_with_confirmation(
        config_path,
        launch_cwd,
        |challenge| {
            eprintln!(
                "Authority recovery will make the failed epoch inert and activate the fresh storage roots in the current config.\nType this exact challenge to continue:\n{challenge}"
            );
            let mut supplied = String::new();
            std::io::stdin()
                .read_line(&mut supplied)
                .map_err(|error| error.to_string())?;
            Ok(supplied)
        },
    )
    .map_err(anyhow::Error::msg)?;
    println!(
        "authority recovery complete: epoch {} -> {}, receipt={}, reconciled={}",
        summary.old_authority_epoch,
        summary.new_authority_epoch,
        summary.receipt_hash,
        summary.reconciled_after_crash
    );
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
            appearance_checks: Some(
                &sigil_tui_app::appearance_diagnostics::appearance_doctor_checks,
            ),
            ..DoctorReportOptions::default()
        },
    )
}

fn build_cli_doctor_support_report(
    config_path: &Path,
    launch_cwd: &Path,
) -> Result<DoctorSupportReportV1> {
    let report = build_cli_doctor_report(config_path, launch_cwd);
    let root_config = RootConfig::load(config_path).ok();
    let redactor = root_config
        .as_ref()
        .map(secret_redactor_for_root_config)
        .unwrap_or_default();
    let mut path_redactions = vec![
        SupportPathRedaction::new(config_path, SupportPathKind::Config),
        SupportPathRedaction::new(launch_cwd, SupportPathKind::Workspace),
    ];
    if let Some(root_config) = root_config.as_ref() {
        let workspace_root =
            resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
        let paths =
            resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
        path_redactions.extend([
            SupportPathRedaction::new(workspace_root, SupportPathKind::Workspace),
            SupportPathRedaction::new(paths.cache_root, SupportPathKind::Cache),
            SupportPathRedaction::new(paths.state_root, SupportPathKind::State),
        ]);
    }
    if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        path_redactions.push(SupportPathRedaction::new(home, SupportPathKind::Home));
    }
    let build = BuildInfo::current();
    let build = SupportBuildInfo::new(build.version, build.git_hash, build.target, build.profile);
    let environment = SupportEnvironmentV1::current();
    project_doctor_support_report_v1(
        &report,
        DoctorSupportProjectionContext {
            generated_at_unix_ms: sigil_runtime::current_unix_time_ms(),
            build: &build,
            environment: &environment,
            redactor: &redactor,
            path_redactions: &path_redactions,
        },
    )
}

fn render_doctor_report(report: &DoctorReport) -> String {
    let mut output = String::from("Sigil doctor\n");
    output.push_str(&format!(
        "cutover: epoch={} authority={} blockers={}\n",
        report.cutover.epoch.as_str(),
        report.cutover.authority.as_str(),
        report.cutover.blockers.len()
    ));
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
    startup_output: ServeStartupOutput,
    shutdown_on_stdin_close: bool,
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

struct ServeOwnerChannelWatcher {
    closed: tokio::sync::oneshot::Receiver<()>,
    thread: JoinHandle<()>,
}

impl ServeOwnerChannelWatcher {
    fn spawn<R>(mut reader: R) -> std::io::Result<Self>
    where
        R: Read + Send + 'static,
    {
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("sigil-serve-owner-channel".to_owned())
            .spawn(move || {
                let mut buffer = [0_u8; 256];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => {
                            let _ = closed_tx.send(());
                            return;
                        }
                        Ok(_) => {}
                    }
                }
            })?;
        Ok(Self { closed, thread })
    }

    async fn wait(&mut self) {
        let _ = (&mut self.closed).await;
    }

    fn reap_if_finished(self) -> Result<()> {
        if self.thread.is_finished() {
            self.thread
                .join()
                .map_err(|_| anyhow::anyhow!("serve owner channel watcher panicked"))?;
        }
        Ok(())
    }
}

fn load_serve_root_config(config_path: &Path) -> RootConfig {
    match RootConfig::load(config_path) {
        Ok(config) => config,
        Err(_) => sigil_runtime::provider_connections::default_setup_root_config(),
    }
}

#[cfg(not(test))]
fn open_serve_protocol_journal(server_root: &Path) -> Result<HttpDurableProtocolJournal> {
    HttpDurableProtocolJournal::open_with_replay_rebuild(
        server_root.join(HTTP_PROTOCOL_JOURNAL_FILE),
        4_096,
    )
    .map_err(Into::into)
}

#[cfg(not(test))]
async fn serve_command(
    config_path: &Path,
    launch_cwd: &Path,
    options: ServeOptions,
    token: Option<&str>,
) -> Result<()> {
    let config = options.http_config();
    let mut plan = build_serve_startup_plan(options.clone(), token)?;
    let root_config = load_serve_root_config(config_path);
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    // The durable HTTP journals encode the exact machine protocol. A breaking
    // protocol revision must use a fresh state root so an unreleased older
    // command envelope cannot make the new server fail closed at startup.
    let server_root = paths.workspace_state_root.join(HTTP_SERVER_STATE_DIR);
    let protocol_journal = std::sync::Arc::new(open_serve_protocol_journal(&server_root)?);
    let disclosure_journal = std::sync::Arc::new(HttpDurableEgressDisclosureJournal::open(
        server_root.join("egress-disclosures.json"),
        4_096,
    )?);
    let command_store = std::sync::Arc::new(HttpDurableCommandStore::open(
        server_root.join("command-identities.json"),
        4_096,
    )?);
    let event_bus = std::sync::Arc::new(HttpLiveEventBus::with_durable_journal(
        256,
        protocol_journal,
    ));
    let lifecycle = build_session_lifecycle_service(&paths);
    // RFC-0062 14.1: one process-scoped scratch lease registry shared by session-delete
    // cleanup, TTL GC and every run tool surface, so leases are observed consistently.
    let scratch_control = sigil_runtime::authority_scratch_control(paths.scratch_root.clone());
    let lifecycle = lifecycle.with_scratch_cleanup(scratch_control.clone());
    let driver = std::sync::Arc::new(HttpProductionRunDriver::new(
        HttpProductionRunDriverOptions::new(config_path, launch_cwd)
            .with_session_lifecycle(lifecycle.clone())
            .with_scratch_control(scratch_control),
        std::sync::Arc::clone(&disclosure_journal),
        std::sync::Arc::clone(&event_bus),
        tokio::runtime::Handle::current(),
    )?);
    let registry = driver.build_registry(command_store)?;
    let session_catalog = std::sync::Arc::new(SessionCatalogProjectionService::new(
        driver.session_lifecycle().cloned().unwrap_or(lifecycle),
        &paths.session_catalog_db,
    ));
    let warm_catalog = std::sync::Arc::clone(&session_catalog);
    match tokio::task::spawn_blocking(move || warm_catalog.reconcile()).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => eprintln!(
            "warning: historical session catalog is unavailable; catalog requests will return 503: {}",
            session_catalog_projection_error_code(&error)
        ),
        Err(_) => eprintln!(
            "warning: historical session catalog is unavailable; catalog requests will return 503: warmup_task_failed"
        ),
    }
    let support_context =
        HttpSupportContext::new(config_path, launch_cwd, BuildInfo::current().into());
    let support_context = match driver.borrowed_configuration_service() {
        Some(service) => support_context.with_borrowed_configuration_service(service),
        None => support_context,
    };
    let server = HttpLocalServer::bind_production(
        config,
        token,
        registry,
        event_bus,
        disclosure_journal,
        session_catalog,
        paths.workspace_id.clone(),
        options.shutdown_on_stdin_close,
    )
    .await?
    .with_support_context(support_context);
    let server = match driver.borrowed_native_save_service() {
        Some(service) => server.with_borrowed_native_save_service(service),
        None => server,
    };
    plan.bind_addr = server.local_addr()?;
    let mut owner_channel = options
        .shutdown_on_stdin_close
        .then(|| ServeOwnerChannelWatcher::spawn(std::io::stdin()))
        .transpose()?;
    match options.startup_output {
        ServeStartupOutput::Text => print!("{}", render_serve_startup_plan(&plan)),
        ServeStartupOutput::Json => {
            let info = server
                .server_info()
                .ok_or_else(|| anyhow::anyhow!("production HTTP server metadata is unavailable"))?;
            print!("{}", render_serve_startup_json(info)?);
        }
    }
    io::stdout().flush()?;
    server
        .serve_until_shutdown(async {
            if let Some(owner_channel) = owner_channel.as_mut() {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    () = owner_channel.wait() => {}
                }
            } else {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await?;
    if let Some(owner_channel) = owner_channel {
        owner_channel.reap_if_finished()?;
    }
    Ok(())
}

fn session_catalog_projection_error_code(error: &SessionCatalogProjectionError) -> &'static str {
    match error {
        SessionCatalogProjectionError::UnsafePath { .. } => "unsafe_path",
        SessionCatalogProjectionError::IncompatibleSchema { .. } => "incompatible_schema",
        SessionCatalogProjectionError::Sqlite { .. } => "sqlite",
        SessionCatalogProjectionError::Source { .. } => "source",
        SessionCatalogProjectionError::IntegerRange { .. } => "integer_range",
        SessionCatalogProjectionError::Encoding { .. } => "encoding",
        SessionCatalogProjectionError::InvalidQuery { .. } => "invalid_query",
        SessionCatalogProjectionError::InvalidCursor { .. } => "invalid_cursor",
        SessionCatalogProjectionError::StaleCursor { .. } => "stale_cursor",
        SessionCatalogProjectionError::ReconcileConflict => "reconcile_conflict",
        SessionCatalogProjectionError::RecoveryBusy => "recovery_busy",
        SessionCatalogProjectionError::Recovery { .. } => "recovery",
    }
}

fn build_session_lifecycle_service(paths: &SigilPaths) -> LocalSessionLifecycleService {
    LocalSessionLifecycleService::new(
        paths.workspace_id.clone(),
        &paths.session_log_dir,
        &paths.session_exports_root,
    )
}

#[cfg(test)]
fn build_session_catalog_service(paths: &SigilPaths) -> SessionCatalogProjectionService {
    SessionCatalogProjectionService::new(
        build_session_lifecycle_service(paths),
        &paths.session_catalog_db,
    )
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
        "Sigil HTTP/SSE adapter\nbind: {}\nauth: {}\nstatus: listening; press Ctrl-C for graceful shutdown\n",
        plan.bind_addr, auth
    )
}

fn render_serve_startup_json(info: &HttpServerInfo) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string(info)?))
}

/// Projects the bounded pending plan artifact for a headless run, if any.
///
/// RFC-0063 9.3: a committed draft without a decision and without a created task means the run
/// must stop at `awaiting_plan_decision`; nothing is auto-accepted or executed.
fn pending_plan_review_artifact(
    session_log_path: &str,
) -> Result<Option<sigil_runtime::machine_protocol::MachinePlanReviewArtifact>> {
    let store = sigil_kernel::JsonlSessionStore::new(session_log_path)?;
    let session = sigil_kernel::Session::load_from_store("", "", store)?;
    let projection = session.plan_artifact_projection();
    for (plan_id, draft) in &projection.plans {
        if projection.latest_decision(plan_id).is_some()
            || projection.task_created_for_plan(plan_id)
        {
            continue;
        }
        return Ok(Some(
            sigil_runtime::machine_protocol::MachinePlanReviewArtifact {
                plan_id: plan_id.as_str().to_owned(),
                plan_hash: draft.plan_hash.clone(),
                summary: draft.summary.clone(),
                step_count: draft.steps.len(),
                target_path_count: draft.target_paths.len(),
                suggested_check_count: draft.suggested_checks.len(),
                risk: draft.risk.clone(),
            },
        ));
    }
    Ok(None)
}

async fn plan_decision_command(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    plan_id: &str,
    plan_hash: &str,
    action: PlanDecisionAction,
) -> Result<String> {
    let root_config = RootConfig::load(config_path)
        .with_context(|| format!("failed to load config at {}", config_path.display()))?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let store = sigil_kernel::JsonlSessionStore::new(session_path)?;
    let session = sigil_kernel::Session::load_from_store("", "", store)
        .with_context(|| format!("failed to load session at {}", session_path.display()))?;
    let action = match action {
        PlanDecisionAction::Run => sigil_runtime::ApplicationPlanAction::Run,
        PlanDecisionAction::Save => sigil_runtime::ApplicationPlanAction::Save,
        PlanDecisionAction::Revise => sigil_runtime::ApplicationPlanAction::Revise,
        PlanDecisionAction::Reject => sigil_runtime::ApplicationPlanAction::Reject,
    };
    let receipt = sigil_runtime::application_plan_decision(
        &root_config,
        &workspace_root,
        session_path,
        session.session_scope_id(),
        &sigil_runtime::ApplicationPlanDecisionCommand {
            plan_id: plan_id.to_owned(),
            expected_plan_hash: plan_hash.to_owned(),
            action,
            permission_grant: None,
        },
    )
    .with_context(|| "plan decision was rejected by the durable session".to_owned())?;
    let rendered = serde_json::to_string_pretty(&CliPlanDecisionReceipt {
        command: "plan_decision",
        plan_id: receipt.plan_id,
        plan_hash: receipt.plan_hash,
        action: match receipt.action {
            sigil_runtime::ApplicationPlanAction::Run => "run",
            sigil_runtime::ApplicationPlanAction::Save => "save",
            sigil_runtime::ApplicationPlanAction::Revise => "revise",
            sigil_runtime::ApplicationPlanAction::Reject => "reject",
        },
        task_id: receipt.task_id,
        task_phase: receipt.task_phase.map(|phase| phase.as_str().to_owned()),
        task_blocker: receipt.task_blocker.map(|blocker| CliTaskBlocker {
            reason_code: blocker.reason_code.as_str().to_owned(),
            summary: blocker.summary,
            retryable: blocker.retryable,
            available_actions: blocker
                .available_actions
                .iter()
                .map(|action| action.as_str().to_owned())
                .collect(),
        }),
    })
    .context("failed to serialize plan decision receipt")?;
    Ok(rendered)
}

#[derive(serde::Serialize)]
struct CliTaskBlocker {
    reason_code: String,
    summary: String,
    retryable: bool,
    available_actions: Vec<String>,
}

#[derive(serde::Serialize)]
struct CliPlanDecisionReceipt {
    command: &'static str,
    plan_id: String,
    plan_hash: String,
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_blocker: Option<CliTaskBlocker>,
}

fn cli_application_run_request(
    config_path: &Path,
    launch_cwd: &Path,
    prompt: String,
    connection: Option<&str>,
    model: Option<&str>,
    session_path: Option<&Path>,
    route_recovery_binding: Option<&str>,
) -> std::result::Result<ApplicationRunRequest, ApplicationRunPrepareError> {
    let mut request = ApplicationRunRequest::non_interactive(
        config_path,
        launch_cwd,
        prompt,
        uuid::Uuid::new_v4().to_string(),
    );
    request.session_path = session_path.map(Path::to_path_buf);
    request.route_recovery_binding = route_recovery_binding.map(str::to_owned);
    match (connection, model) {
        (None, None) => {}
        (Some(connection), Some(model)) => {
            request.model_connection_id = Some(
                sigil_kernel::ConnectionId::new(connection.to_owned()).map_err(|error| {
                    ApplicationRunPrepareError::InvalidInvocation {
                        message: error.to_string(),
                    }
                })?,
            );
            request.model_name = Some(model.to_owned());
        }
        _ => {
            return Err(ApplicationRunPrepareError::InvalidInvocation {
                message: "--connection and --model must be supplied together".to_owned(),
            });
        }
    }
    Ok(request)
}

/// RFC-0071 R71.6: boot-time epoch selection. Delegates to the shared runtime attachment
/// (CLI headless/machine must not re-implement the epoch decision).
fn attach_boot_cutover(
    services: ApplicationRunServices,
    config_path: &Path,
    launch_cwd: &Path,
) -> Result<ApplicationRunServices> {
    // RFC-0071 R71.6: one-call boot attach (epoch + authority composition) shared by every
    // surface; CLI surfaces never re-implement the decision or the composition.
    sigil_runtime::r71_authority_composition::attach_boot_authority_to_services(
        services,
        config_path,
        launch_cwd,
    )
    .map_err(anyhow::Error::new)
}

async fn run_command(
    config_path: &Path,
    launch_cwd: &Path,
    prompt: String,
    connection: Option<&str>,
    model: Option<&str>,
    session_path: Option<&Path>,
    route_recovery_binding: Option<&str>,
) -> Result<()> {
    let disclosure_presenter: std::sync::Arc<dyn sigil_kernel::EgressDisclosurePresenter> =
        std::sync::Arc::new(crate::egress_disclosure::CliEgressDisclosurePresenter::stderr());
    // RFC-0071 R71.6: parse the request first (config read happens there, cancellation-aware
    // in machine mode), then attach the boot epoch/authority before any run is prepared.
    let request = cli_application_run_request(
        config_path,
        launch_cwd,
        prompt,
        connection,
        model,
        session_path,
        route_recovery_binding,
    )?;
    let services = attach_boot_cutover(
        ApplicationRunServices::new(disclosure_presenter),
        config_path,
        launch_cwd,
    )?;
    let prepared = prepare_application_run(request, &services).await?;
    let (execution, _control) = prepared.into_parts();
    let mut handler = StdoutEventHandler;
    let mut approval_handler = AutoApproveHandler;
    let output = execution
        .execute(&mut handler, &mut approval_handler)
        .await?;
    if !output.agent_output.result.final_text.is_empty() {
        println!();
    }
    eprintln!("session log: {}", output.session_log_path.display());
    Ok(())
}

#[cfg(not(test))]
async fn run_machine_command(
    config_path: &Path,
    launch_cwd: &Path,
    prompt: String,
    output: RunOutput,
    connection: Option<&str>,
    model: Option<&str>,
    session_path: Option<&Path>,
    route_recovery_binding: Option<&str>,
) -> MachineExitCode {
    let mut stdout = io::stdout();
    let mut cancellation =
        Box::pin(async { tokio::signal::ctrl_c().await.map_err(anyhow::Error::from) });
    tokio::select! {
        biased;
        trigger = &mut cancellation => {
            let (error, exit) = pre_start_cancellation_error(trigger.is_err());
            eprintln!("sigil run: {}", error.message);
            return write_machine_terminal(&mut stdout, MachineRecord::error(error), exit);
        }
        () = tokio::task::yield_now() => {}
    }
    run_machine_command_with_route_and_cancellation(
        config_path,
        launch_cwd,
        prompt,
        output,
        connection,
        model,
        session_path,
        route_recovery_binding,
        &mut stdout,
        cancellation,
    )
    .await
}

#[cfg(test)]
async fn run_machine_command_with_writer<W>(
    config_path: &Path,
    launch_cwd: &Path,
    prompt: String,
    output: RunOutput,
    writer: &mut W,
) -> MachineExitCode
where
    W: Write + Send,
{
    run_machine_command_with_route_and_cancellation(
        config_path,
        launch_cwd,
        prompt,
        output,
        None,
        None,
        None,
        None,
        writer,
        std::future::pending(),
    )
    .await
}

#[cfg(test)]
async fn run_machine_command_with_cancellation<W, F>(
    config_path: &Path,
    launch_cwd: &Path,
    prompt: String,
    output: RunOutput,
    writer: &mut W,
    cancellation: F,
) -> MachineExitCode
where
    W: Write + Send,
    F: Future<Output = Result<()>> + Send,
{
    run_machine_command_with_route_and_cancellation(
        config_path,
        launch_cwd,
        prompt,
        output,
        None,
        None,
        None,
        None,
        writer,
        cancellation,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_machine_command_with_route_and_cancellation<W, F>(
    config_path: &Path,
    launch_cwd: &Path,
    prompt: String,
    output: RunOutput,
    connection: Option<&str>,
    model: Option<&str>,
    session_path: Option<&Path>,
    route_recovery_binding: Option<&str>,
    writer: &mut W,
    cancellation: F,
) -> MachineExitCode
where
    W: Write + Send,
    F: Future<Output = Result<()>> + Send,
{
    debug_assert!(output != RunOutput::Text);
    let disclosure_presenter: std::sync::Arc<dyn sigil_kernel::EgressDisclosurePresenter> =
        std::sync::Arc::new(crate::egress_disclosure::CliEgressDisclosurePresenter::stderr());
    let mut cancellation = Box::pin(cancellation);
    // RFC-0071 R71.6: parse the request first (config read happens here, cancellation-aware),
    // then attach the boot epoch/authority before any run is prepared.
    let request = match cli_application_run_request(
        config_path,
        launch_cwd,
        prompt,
        connection,
        model,
        session_path,
        route_recovery_binding,
    ) {
        Ok(request) => request,
        Err(error) => {
            let machine_error = machine_error_from_prepare(&error);
            return write_machine_terminal(
                writer,
                MachineRecord::error(machine_error.clone()),
                MachineExitCode::for_error(machine_error.code),
            );
        }
    };
    let services = match attach_boot_cutover(
        ApplicationRunServices::new(disclosure_presenter),
        config_path,
        launch_cwd,
    ) {
        Ok(services) => services,
        Err(_error) => {
            // Machine protocol: a boot guard failure is a closed ConfigurationInvalid before
            // any run starts; the message never leaks raw config paths.
            let error = MachineError {
                code: MachineErrorCode::ConfigurationInvalid,
                message: "application boot failed before the run started".to_owned(),
                retryable: false,
                allowed_actions: vec![],
                recovery_binding: None,
            };
            return write_machine_terminal(
                writer,
                MachineRecord::error(error),
                MachineExitCode::InvalidInput,
            );
        }
    };
    let mut preparation = Box::pin(prepare_application_run(request, &services));
    let prepared = tokio::select! {
        biased;
        trigger = &mut cancellation => {
            let (error, exit) = pre_start_cancellation_error(trigger.is_err());
            eprintln!("sigil run: {}", error.message);
            return write_machine_terminal(writer, MachineRecord::error(error), exit);
        }
        prepared = &mut preparation => prepared,
    };
    drop(preparation);
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let machine_error = machine_error_from_prepare(&error);
            eprintln!("sigil run: {error}");
            return write_machine_terminal(
                writer,
                MachineRecord::error(machine_error.clone()),
                MachineExitCode::for_error(machine_error.code),
            );
        }
    };
    let session_id = prepared.session_id().to_owned();
    let run_id = prepared.run_id().to_owned();
    let Some(session_log_path) = prepared.session_log_path().to_str().map(str::to_owned) else {
        eprintln!("sigil run: durable session path is not valid UTF-8");
        return write_machine_terminal(
            writer,
            MachineRecord::error(MachineError::new(
                MachineErrorCode::Internal,
                "durable session path cannot be represented by the machine protocol",
                false,
            )),
            MachineExitCode::ExecutionFailed,
        );
    };
    let (execution, control) = prepared.into_parts();
    let mut handler = MachineRunEventHandler { output, writer };
    let mut approval_handler = AutoApproveHandler;
    let mut execution = Box::pin(execution.execute(&mut handler, &mut approval_handler));
    let mut cancellation_ticket = None;
    let mut cancellation_trigger_failed = false;
    let mut execution_joined = true;
    let executed = tokio::select! {
        biased;
        trigger = &mut cancellation => {
            cancellation_trigger_failed = trigger.is_err();
            let reason = if cancellation_trigger_failed {
                "machine cancellation signal watcher failed"
            } else {
                "machine run interrupted by SIGINT"
            };
            match control.request_cancellation(reason, None, || {}) {
                Ok(ticket) => cancellation_ticket = Some(ticket),
                Err(error) => cancellation_ticket = error.into_ticket(),
            }
            let join_timeout = cancellation_ticket
                .as_ref()
                .map_or(std::time::Duration::from_secs(5), |ticket| {
                    ticket.remaining_timeout()
                });
            match tokio::time::timeout(join_timeout, execution.as_mut()).await {
                Ok(result) => result,
                Err(_) => {
                    execution_joined = false;
                    Err(anyhow::anyhow!("application run did not join before cancellation deadline"))
                }
            }
        }
        result = &mut execution => result,
    };
    drop(execution);
    if let Some(ticket) = cancellation_ticket {
        let finalized = control
            .finalize_cancellation(ticket, execution_joined, &mut handler)
            .await;
        return match finalized {
            Ok(sigil_kernel::RunCancellationTerminalOutcome::Cancelled)
                if !cancellation_trigger_failed =>
            {
                write_machine_terminal(
                    handler.writer,
                    MachineRecord::result(MachineRunResult {
                        session_id,
                        run_id,
                        status: MachineRunStatus::Cancelled,
                        final_text: String::new(),
                        route_transition: None,
                        session_log_path,
                        plan_review: None,
                    }),
                    MachineExitCode::Cancelled,
                )
            }
            Ok(sigil_kernel::RunCancellationTerminalOutcome::Cancelled) => {
                eprintln!("sigil run: cancellation signal watcher failed");
                write_machine_terminal(
                    handler.writer,
                    MachineRecord::error(MachineError::new(
                        MachineErrorCode::Internal,
                        "application run supervision failed",
                        false,
                    )),
                    MachineExitCode::ExecutionFailed,
                )
            }
            Ok(sigil_kernel::RunCancellationTerminalOutcome::Interrupted) | Err(_) => {
                eprintln!("sigil run: application run cancellation did not reach clean quiescence");
                write_machine_terminal(
                    handler.writer,
                    MachineRecord::error(MachineError::new(
                        MachineErrorCode::ExecutionFailed,
                        "application run cancellation was interrupted",
                        false,
                    )),
                    MachineExitCode::ExecutionFailed,
                )
            }
        };
    }
    match executed {
        Ok(run) => {
            let mut status = match run.terminal_status {
                ApplicationRunTerminalStatus::Succeeded => MachineRunStatus::Succeeded,
                ApplicationRunTerminalStatus::Interrupted
                | ApplicationRunTerminalStatus::Blocked => MachineRunStatus::Failed,
                ApplicationRunTerminalStatus::AwaitingUserInput => {
                    MachineRunStatus::AwaitingUserInput
                }
            };
            // RFC-0063 9.3: a headless run that committed a plan draft without a decision must
            // terminate as `awaiting_plan_decision`, never auto-accept or execute.
            let plan_review = match pending_plan_review_artifact(&session_log_path) {
                Ok(artifact) => artifact,
                Err(error) => {
                    eprintln!("sigil run: pending plan artifact projection failed: {error:#}");
                    None
                }
            };
            if plan_review.is_some() {
                status = MachineRunStatus::AwaitingPlanDecision;
            }
            let result = MachineRunResult {
                session_id: run.session_id,
                run_id: run.run_id,
                status,
                final_text: run.agent_output.result.final_text,
                route_transition: Some(
                    sigil_runtime::application_run::application_public_route_transition(
                        &run.route_transition,
                    ),
                ),
                session_log_path,
                plan_review,
            };
            write_machine_terminal(
                handler.writer,
                MachineRecord::result(result),
                MachineExitCode::for_status(status),
            )
        }
        Err(_error) => {
            let error = MachineError::new(
                MachineErrorCode::ExecutionFailed,
                "application run execution failed",
                false,
            );
            eprintln!("sigil run: application run execution failed");
            write_machine_terminal(
                handler.writer,
                MachineRecord::error(error),
                MachineExitCode::ExecutionFailed,
            )
        }
    }
}

fn pre_start_cancellation_error(failed: bool) -> (MachineError, MachineExitCode) {
    if failed {
        (
            MachineError::new(
                MachineErrorCode::Internal,
                "application run supervision failed before startup completed",
                false,
            ),
            MachineExitCode::ExecutionFailed,
        )
    } else {
        (
            MachineError::new(
                MachineErrorCode::Cancelled,
                "application run was cancelled before startup completed",
                false,
            ),
            MachineExitCode::Cancelled,
        )
    }
}

struct MachineRunEventHandler<'a, W> {
    output: RunOutput,
    writer: &'a mut W,
}

impl<W> ApplicationRunEventHandler for MachineRunEventHandler<'_, W>
where
    W: Write,
{
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
        if self.output == RunOutput::Jsonl {
            write_machine_record(self.writer, &MachineRecord::event(event))?;
        }
        Ok(())
    }
}

fn machine_error_from_prepare(error: &ApplicationRunPrepareError) -> MachineError {
    use sigil_kernel::PublicRouteRecoveryAction as Action;

    let code = match error.class() {
        ApplicationRunPrepareErrorClass::InvalidInvocation => MachineErrorCode::InvalidInvocation,
        ApplicationRunPrepareErrorClass::Configuration => MachineErrorCode::ConfigurationInvalid,
        ApplicationRunPrepareErrorClass::ConnectionConfigInvalid => {
            MachineErrorCode::ConnectionConfigInvalid
        }
        ApplicationRunPrepareErrorClass::ProviderUnavailable => {
            MachineErrorCode::ProviderUnavailable
        }
        ApplicationRunPrepareErrorClass::ModelRouteNotConfigured => {
            MachineErrorCode::ModelRouteNotConfigured
        }
        ApplicationRunPrepareErrorClass::SessionRouteConfirmationRequired => {
            MachineErrorCode::SessionRouteConfirmationRequired
        }
        ApplicationRunPrepareErrorClass::SessionRouteSelectionRequired => {
            MachineErrorCode::SessionRouteSelectionRequired
        }
        ApplicationRunPrepareErrorClass::SessionAlreadyActive => {
            MachineErrorCode::SessionAlreadyActive
        }
        ApplicationRunPrepareErrorClass::SessionWriterBusy => MachineErrorCode::SessionWriterBusy,
        ApplicationRunPrepareErrorClass::SessionStreamInvalid => {
            MachineErrorCode::SessionStreamInvalid
        }
        ApplicationRunPrepareErrorClass::Execution => MachineErrorCode::ExecutionFailed,
        ApplicationRunPrepareErrorClass::Internal => MachineErrorCode::Internal,
    };
    let (retryable, allowed_actions) = match error.class() {
        ApplicationRunPrepareErrorClass::SessionRouteConfirmationRequired => (
            true,
            vec![
                Action::ConfirmCurrentRoute,
                Action::RepairConnection,
                Action::SelectReplacement,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
        ),
        ApplicationRunPrepareErrorClass::SessionRouteSelectionRequired => (
            true,
            vec![
                Action::RepairConnection,
                Action::SelectReplacement,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
        ),
        ApplicationRunPrepareErrorClass::ModelRouteNotConfigured
        | ApplicationRunPrepareErrorClass::Configuration
        | ApplicationRunPrepareErrorClass::ConnectionConfigInvalid => (
            false,
            vec![
                Action::RepairConnection,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
        ),
        ApplicationRunPrepareErrorClass::ProviderUnavailable => (
            true,
            vec![
                Action::RetryProvider,
                Action::RepairConnection,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
        ),
        ApplicationRunPrepareErrorClass::SessionAlreadyActive => (
            true,
            vec![
                Action::RetrySessionAttach,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
        ),
        ApplicationRunPrepareErrorClass::SessionWriterBusy => (
            true,
            vec![Action::StartNewSession, Action::BackToSessionLibrary],
        ),
        ApplicationRunPrepareErrorClass::SessionStreamInvalid => (
            false,
            vec![Action::StartNewSession, Action::BackToSessionLibrary],
        ),
        ApplicationRunPrepareErrorClass::InvalidInvocation
        | ApplicationRunPrepareErrorClass::Execution
        | ApplicationRunPrepareErrorClass::Internal => (false, Vec::new()),
    };
    MachineError::new(code, error.to_string(), retryable)
        .with_allowed_actions(allowed_actions)
        .with_recovery_binding(error.recovery_binding())
}

fn write_machine_terminal<W>(
    writer: &mut W,
    record: MachineRecord,
    intended_exit: MachineExitCode,
) -> MachineExitCode
where
    W: Write,
{
    match write_machine_record(writer, &record) {
        Ok(()) => intended_exit,
        Err(_) => {
            eprintln!("sigil run: failed to write machine output");
            MachineExitCode::ExecutionFailed
        }
    }
}

fn write_machine_record(writer: &mut impl Write, record: &MachineRecord) -> Result<()> {
    serde_json::to_writer(&mut *writer, record)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
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

#[derive(Debug, Default, PartialEq, Eq)]
struct RenderedOutput {
    stdout: String,
    stderr: String,
    stop: bool,
}

enum StreamRenderEvent {
    TextDelta(String),
    ReasoningDelta(String),
    Usage(Box<UsageStats>),
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
        ProviderChunk::Usage(usage) => {
            render_stream_event(StreamRenderEvent::Usage(Box::new(usage)))
        }
        ProviderChunk::Done => render_stream_event(StreamRenderEvent::Done),
        _ => RenderedOutput::default(),
    }
}

fn render_run_event(event: RunEvent) -> RenderedOutput {
    render_public_run_event(event.into())
}

fn render_public_run_event(event: PublicRunEventKind) -> RenderedOutput {
    match event {
        PublicRunEventKind::TextDelta { text } => {
            render_stream_event(StreamRenderEvent::TextDelta(text))
        }
        PublicRunEventKind::ReasoningDelta { text } => {
            render_stream_event(StreamRenderEvent::ReasoningDelta(text))
        }
        PublicRunEventKind::ToolCallStarted { call } => RenderedOutput {
            stderr: format!("\n[tool:start] {} ({})\n", call.name, call.id),
            ..RenderedOutput::default()
        },
        PublicRunEventKind::ToolCallArgsDelta { id, delta } => RenderedOutput {
            stderr: format!("[tool:args:{id}] {delta}\n"),
            ..RenderedOutput::default()
        },
        PublicRunEventKind::ToolCallCompleted { call } => RenderedOutput {
            stderr: format!("[tool:complete] {} ({})\n", call.name, call.id),
            ..RenderedOutput::default()
        },
        PublicRunEventKind::ApprovalRequested {
            call,
            spec,
            subjects,
            network_effect,
            local_policy_decision,
            network_policy_decision,
            source_policy_decision,
            risk,
            preview,
            ..
        } => {
            let local_policy_decision =
                local_policy_decision.unwrap_or(sigil_kernel::ApprovalMode::Allow);
            let network_policy_decision =
                network_policy_decision.unwrap_or(sigil_kernel::ApprovalMode::Allow);
            let source_policy_decision =
                source_policy_decision.unwrap_or(sigil_kernel::ApprovalMode::Allow);
            let final_policy_decision = strictest_approval_mode([
                local_policy_decision,
                network_policy_decision,
                source_policy_decision,
            ]);
            let mut stderr = format!(
                "[tool:approval] {} ({}) {} {} network={} risk={} policy=local:{} network:{} source:{} final:{} subjects={}\n",
                call.name,
                call.id,
                spec.category.as_str(),
                spec.access.as_str(),
                network_effect.map_or("none", sigil_kernel::NetworkEffect::as_str),
                risk.map_or("unknown", permission_risk_label),
                local_policy_decision.as_str(),
                network_policy_decision.as_str(),
                source_policy_decision.as_str(),
                final_policy_decision.as_str(),
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
        PublicRunEventKind::ApprovalResolved {
            call_id,
            approved,
            reason,
            approval_request_id: _,
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
        PublicRunEventKind::ToolResult { result } => RenderedOutput {
            stderr: format!(
                "[tool:result] {} error={} {}\n",
                result.tool_name,
                result.is_error(),
                result.content
            ),
            ..RenderedOutput::default()
        },
        PublicRunEventKind::ToolProgress { progress } => {
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
        PublicRunEventKind::Usage { usage } => {
            render_stream_event(StreamRenderEvent::Usage(Box::new(usage)))
        }
        PublicRunEventKind::Notice { message } => RenderedOutput {
            stderr: format!("[notice] {message}\n"),
            ..RenderedOutput::default()
        },
        PublicRunEventKind::UserInputChanged {
            request,
            status: sigil_kernel::UserInputStatusV1::Requested,
            ..
        } => {
            let questions = request
                .questions
                .iter()
                .map(|question| format!("- {}", question.question))
                .collect::<Vec<_>>()
                .join("\n");
            RenderedOutput {
                stderr: format!("[input:required] {}\n{questions}\n", request.prompt),
                ..RenderedOutput::default()
            }
        }
        PublicRunEventKind::RouteRecoveryRequired { code, .. } => {
            let message = match code {
                sigil_kernel::PublicRouteRecoveryCode::SessionRouteConfirmationRequired => {
                    "the saved session route needs explicit confirmation"
                }
                sigil_kernel::PublicRouteRecoveryCode::SessionRouteSelectionRequired => {
                    "the saved session route is unavailable; select a replacement"
                }
                sigil_kernel::PublicRouteRecoveryCode::ModelRouteNotConfigured => {
                    "no model route is configured"
                }
                sigil_kernel::PublicRouteRecoveryCode::ConnectionConfigInvalid => {
                    "the connection configuration is invalid"
                }
                sigil_kernel::PublicRouteRecoveryCode::ProviderUnavailable => {
                    "the provider is temporarily unavailable"
                }
                sigil_kernel::PublicRouteRecoveryCode::SessionAlreadyActive => {
                    "the session is already active in another surface"
                }
                sigil_kernel::PublicRouteRecoveryCode::SessionWriterBusy => {
                    "the session writer is busy"
                }
                sigil_kernel::PublicRouteRecoveryCode::SessionStreamInvalid => {
                    "the session stream is invalid"
                }
            };
            RenderedOutput {
                stderr: format!("[recovery] {message}\n"),
                ..RenderedOutput::default()
            }
        }
        PublicRunEventKind::ProviderTurnRecoveryChanged { recovery } => {
            let phase = match recovery.phase {
                sigil_kernel::PublicProviderTurnRecoveryPhaseV1::Waiting => "waiting",
                sigil_kernel::PublicProviderTurnRecoveryPhaseV1::Recovering => "recovering",
                sigil_kernel::PublicProviderTurnRecoveryPhaseV1::Blocked => "blocked",
                sigil_kernel::PublicProviderTurnRecoveryPhaseV1::Paused => "paused",
            };
            let reason = recovery
                .reason_code
                .as_deref()
                .map(|code| format!(" reason={code}"))
                .unwrap_or_default();
            RenderedOutput {
                stderr: format!(
                    "[provider:recovery] {phase} retry={}/{}{}\n",
                    recovery.retry_count, recovery.max_transport_retries, reason
                ),
                ..RenderedOutput::default()
            }
        }
        PublicRunEventKind::ProviderTurnPartialOutputDiscarded { output } => {
            let mut discarded = Vec::new();
            if output.text_discarded {
                discarded.push("text");
            }
            if output.reasoning_discarded {
                discarded.push("reasoning");
            }
            if output.tool_request_discarded {
                discarded.push("tool request");
            }
            RenderedOutput {
                stderr: format!(
                    "[provider:recovery] discarded partial {}; replacement output will follow\n",
                    discarded.join(", ")
                ),
                ..RenderedOutput::default()
            }
        }
        PublicRunEventKind::RunBlocked { reason } => RenderedOutput {
            stderr: format!("[run:blocked] {reason}\n"),
            ..RenderedOutput::default()
        },
        PublicRunEventKind::RunPaused { reason } => RenderedOutput {
            stderr: format!("[run:paused] {reason}\n"),
            ..RenderedOutput::default()
        },
        PublicRunEventKind::RunInterrupted { reason } => RenderedOutput {
            stderr: format!("[run:interrupted] {reason}\n"),
            ..RenderedOutput::default()
        },
        PublicRunEventKind::RouteTransition { .. }
        | PublicRunEventKind::RunStarted { .. }
        | PublicRunEventKind::TaskRunStarted { .. }
        | PublicRunEventKind::RunFinished { .. }
        | PublicRunEventKind::RunAwaitingUserInput { .. }
        | PublicRunEventKind::TaskRunFinished { .. }
        | PublicRunEventKind::TaskRoutingChanged { .. }
        | PublicRunEventKind::ConversationRouteChanged { .. }
        | PublicRunEventKind::PlanReviewChanged { .. }
        | PublicRunEventKind::UserInputChanged { .. }
        | PublicRunEventKind::TaskPhaseChanged { .. }
        | PublicRunEventKind::TaskExecutionAdmitted { .. }
        | PublicRunEventKind::TaskPlanUpdated { .. }
        | PublicRunEventKind::TaskChecklistUpdated { .. }
        | PublicRunEventKind::TaskBatchChanged { .. }
        | PublicRunEventKind::TaskStepChanged { .. }
        | PublicRunEventKind::IntegrationLaneChanged { .. }
        | PublicRunEventKind::RunFailed { .. }
        | PublicRunEventKind::RunCancelled
        | PublicRunEventKind::ContinuationState { .. }
        | PublicRunEventKind::TerminalLifecycle { .. }
        | PublicRunEventKind::Control { .. }
        | PublicRunEventKind::AssistantMessage { .. } => RenderedOutput::default(),
    }
}

fn strictest_approval_mode(modes: [sigil_kernel::ApprovalMode; 3]) -> sigil_kernel::ApprovalMode {
    if modes.contains(&sigil_kernel::ApprovalMode::Deny) {
        sigil_kernel::ApprovalMode::Deny
    } else if modes.contains(&sigil_kernel::ApprovalMode::Ask) {
        sigil_kernel::ApprovalMode::Ask
    } else {
        sigil_kernel::ApprovalMode::Allow
    }
}

fn permission_risk_label(risk: sigil_kernel::PermissionRisk) -> &'static str {
    match risk {
        sigil_kernel::PermissionRisk::Low => "low",
        sigil_kernel::PermissionRisk::Medium => "medium",
        sigil_kernel::PermissionRisk::High => "high",
        sigil_kernel::PermissionRisk::Destructive => "destructive",
        sigil_kernel::PermissionRisk::Protected => "protected",
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

impl ApplicationRunEventHandler for StdoutEventHandler {
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
        emit_rendered_output(render_public_run_event(event.event));
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/main_tests.rs"]
mod tests;
