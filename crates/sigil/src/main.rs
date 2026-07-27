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

use anyhow::Result;
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
use sigil_runtime::{LocalSessionLifecycleService, SessionCatalogProjectionService, SigilPaths};

const HTTP_SERVER_STATE_DIR: &str = "http-server-v2";

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

impl From<BuildInfo> for SupportBuildInfo {
    fn from(value: BuildInfo) -> Self {
        Self::new(value.version, value.git_hash, value.target, value.profile)
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
        #[arg(long, value_enum, default_value = "text")]
        output: RunOutput,
        #[arg(long, requires = "model")]
        connection: Option<String>,
        #[arg(long, requires = "connection")]
        model: Option<String>,
    },
    Resume {
        session: Option<String>,
    },
    Doctor {
        #[arg(long, value_enum, default_value = "text")]
        output: DoctorOutput,
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
    /// Hidden developer adapter for explicit provider-backed acceptance campaigns.
    #[command(name = "model-eval", hide = true)]
    ModelEval {
        #[arg(long = "case", required = true)]
        cases: Vec<String>,
        #[arg(long, default_value_t = 1)]
        repetitions: u32,
        #[arg(long = "max-cost-usd")]
        max_cost_usd: String,
        #[arg(long = "timeout-secs", default_value_t = 300)]
        timeout_secs: u64,
        #[arg(long = "output-dir")]
        output_dir: PathBuf,
        #[arg(long = "orchestration-route-contract")]
        orchestration_route_contract: Option<PathBuf>,
    },
    /// Hidden release-owner adapter for a frozen orchestration candidate.
    #[command(name = "model-eval-route-contract", hide = true)]
    ModelEvalRouteContract {
        #[arg(long = "case", default_value = "orchestration-v1")]
        cases: Vec<String>,
        #[arg(long)]
        output: PathBuf,
    },
    /// Hidden release-owner adapter for deriving a path-free rollout sidecar.
    #[command(name = "model-eval-rollout-manifest", hide = true)]
    ModelEvalRolloutManifest {
        #[arg(long = "report")]
        report: PathBuf,
        #[arg(long)]
        output: PathBuf,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum ServeStartupOutput {
    #[default]
    Text,
    Json,
}

#[derive(Subcommand)]
enum TokenizerCommand {
    /// Explicitly download and checksum-verify the public tokenizer needed for DeepSeek V4 Flash portable compaction.
    Install { profile: String },
}

#[cfg(not(test))]
fn main() -> ExitCode {
    tracing_subscriber::fmt::init();
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
    let result = runtime.block_on(run_main());
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(test))]
async fn run_main() -> Result<u8> {
    let cli = Cli::parse();
    if cli.show_version {
        print!("{}", render_version(BuildInfo::current()));
        return Ok(0);
    }
    let Some(command) = cli.command else {
        sigil_tui::launcher::run_tui_with_build_info(cli.config, BuildInfo::current().into())?;
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
        } => {
            run_command(
                &config_path,
                &cwd,
                prompt,
                connection.as_deref(),
                model.as_deref(),
            )
            .await?
        }
        Commands::Run {
            prompt,
            output,
            connection,
            model,
        } => {
            let code = run_machine_command(
                &config_path,
                &cwd,
                prompt,
                output,
                connection.as_deref(),
                model.as_deref(),
            )
            .await
            .as_i32();
            return Ok(u8::try_from(code).expect("machine exit codes must fit in u8"));
        }
        Commands::Resume { session } => {
            sigil_tui::launcher::run_tui_resume_with_build_info(
                cli.config,
                session,
                BuildInfo::current().into(),
            )?;
        }
        Commands::Doctor { output } => doctor_command(&config_path, &cwd, output)?,
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
        Commands::ModelEval {
            cases,
            repetitions,
            max_cost_usd,
            timeout_secs,
            output_dir,
            orchestration_route_contract,
        } => {
            model_eval_command(
                &config_path,
                &cwd,
                cases,
                repetitions,
                &max_cost_usd,
                timeout_secs,
                output_dir,
                orchestration_route_contract,
            )
            .await?;
        }
        Commands::ModelEvalRouteContract { cases, output } => {
            model_eval_route_contract_command(
                &config_path,
                &cwd,
                &BuildInfo::current(),
                cases,
                output,
            )?;
        }
        Commands::ModelEvalRolloutManifest { report, output } => {
            model_eval_rollout_manifest_command(&cwd, report, output)?;
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
fn model_eval_route_contract_command(
    config_path: &Path,
    launch_cwd: &Path,
    build_info: &BuildInfo,
    cases: Vec<String>,
    output: PathBuf,
) -> Result<()> {
    let fixture_roots = resolve_model_eval_fixture_roots(launch_cwd, &cases)?;
    let output = if output.is_absolute() {
        output
    } else {
        launch_cwd.join(output)
    };
    let contract = sigil_runtime::model_eval::build_model_eval_orchestration_route_contract(
        &sigil_runtime::model_eval::ModelEvalRouteContractBuildRequest {
            config_path: config_path.to_path_buf(),
            fixture_roots,
        },
    )?;
    if contract.sigil_commit != build_info.git_hash {
        anyhow::bail!(
            "candidate CLI and runtime build metadata disagree; rebuild the frozen binary"
        );
    }
    sigil_runtime::model_eval::write_model_eval_orchestration_route_contract(&contract, &output)?;
    println!("wrote {}", output.display());
    Ok(())
}

#[cfg(not(test))]
fn model_eval_rollout_manifest_command(
    launch_cwd: &Path,
    report: PathBuf,
    output: PathBuf,
) -> Result<()> {
    let report = if report.is_absolute() {
        report
    } else {
        launch_cwd.join(report)
    };
    let output = if output.is_absolute() {
        output
    } else {
        launch_cwd.join(output)
    };
    let report = sigil_runtime::load_orchestration_eval_report_manifest(&report)?;
    let manifest = sigil_runtime::build_orchestration_rollout_manifest(&report)?;
    sigil_runtime::write_orchestration_rollout_manifest(&manifest, &output)?;
    println!("wrote {}", output.display());
    Ok(())
}

#[cfg(not(test))]
async fn model_eval_command(
    config_path: &Path,
    launch_cwd: &Path,
    cases: Vec<String>,
    repetitions: u32,
    max_cost_usd: &str,
    timeout_secs: u64,
    output_dir: PathBuf,
    orchestration_route_contract: Option<PathBuf>,
) -> Result<()> {
    let fixture_roots = resolve_model_eval_fixture_roots(launch_cwd, &cases)?;
    let output_dir = if output_dir.is_absolute() {
        output_dir
    } else {
        launch_cwd.join(output_dir)
    };
    let orchestration_route_contract = orchestration_route_contract
        .map(|path| load_model_eval_orchestration_route_contract(launch_cwd, &path))
        .transpose()?;
    let disclosure_presenter: std::sync::Arc<dyn sigil_kernel::EgressDisclosurePresenter> =
        std::sync::Arc::new(crate::egress_disclosure::CliEgressDisclosurePresenter::stderr());
    let services = ApplicationRunServices::new(disclosure_presenter);
    let campaign = sigil_runtime::model_eval::run_model_eval_campaign(
        sigil_runtime::model_eval::ModelEvalCampaignRequest {
            config_path: config_path.to_path_buf(),
            fixture_roots,
            orchestration_route_contract,
            repetitions,
            max_cost_microusd: parse_model_eval_cost_microusd(max_cost_usd)?,
            campaign_timeout: std::time::Duration::from_secs(timeout_secs),
            output_dir,
        },
        &services,
    )
    .await?;
    let manifest_path = campaign.output_dir.join("manifest.json");
    let manifest: sigil_kernel::ModelEvalReportManifestV3 =
        serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    let orchestration_manifest = if campaign.orchestration_route_contract.is_some() {
        let orchestration_manifest_path = campaign.output_dir.join("orchestration/manifest.json");
        Some(serde_json::from_slice::<
            sigil_kernel::OrchestrationEvalReportManifestV1,
        >(&std::fs::read(orchestration_manifest_path)?)?)
    } else {
        None
    };
    println!(
        "wrote {}",
        campaign.output_dir.join("results.jsonl").display()
    );
    println!("wrote {}", manifest_path.display());
    println!("wrote {}", campaign.output_dir.join("summary.md").display());
    if campaign.orchestration_route_contract.is_some() {
        let orchestration_dir = campaign.output_dir.join("orchestration");
        println!(
            "wrote {}",
            orchestration_dir.join("results.jsonl").display()
        );
        println!(
            "wrote {}",
            orchestration_dir.join("manifest.json").display()
        );
        println!("wrote {}", orchestration_dir.join("summary.md").display());
    }
    validate_model_eval_manifest(&manifest)?;
    if let Some(manifest) = &orchestration_manifest {
        validate_orchestration_eval_manifest(manifest)?;
    }
    Ok(())
}

fn load_model_eval_orchestration_route_contract(
    launch_cwd: &Path,
    requested_path: &Path,
) -> Result<sigil_runtime::model_eval::ModelEvalOrchestrationRouteContractV1> {
    let path = if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        launch_cwd.join(requested_path)
    };
    let metadata = std::fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 64 * 1024 {
        anyhow::bail!("orchestration route contract must be a regular file no larger than 64 KiB");
    }
    let bytes = std::fs::read(&path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow::anyhow!("orchestration route contract is not UTF-8"))?;
    toml::from_str(text).map_err(|_| anyhow::anyhow!("orchestration route contract is invalid"))
}

fn validate_model_eval_manifest(manifest: &sigil_kernel::ModelEvalReportManifestV3) -> Result<()> {
    if manifest.requested_repetitions == 0
        || manifest.provider_admitted_repetitions != manifest.requested_repetitions
        || manifest.completed_repetitions != manifest.requested_repetitions
        || manifest.skipped_repetitions != 0
        || manifest.accepted_repetitions != manifest.requested_repetitions
    {
        anyhow::bail!(
            "model eval acceptance failed: requested {}, provider-admitted {}, completed {}, skipped {}, accepted {}",
            manifest.requested_repetitions,
            manifest.provider_admitted_repetitions,
            manifest.completed_repetitions,
            manifest.skipped_repetitions,
            manifest.accepted_repetitions,
        );
    }
    Ok(())
}

fn validate_orchestration_eval_manifest(
    manifest: &sigil_kernel::OrchestrationEvalReportManifestV1,
) -> Result<()> {
    if manifest.route_gates.is_empty() {
        anyhow::bail!("orchestration eval acceptance failed: no route gates were produced");
    }
    let rejected = manifest
        .route_gates
        .iter()
        .filter(|gate| gate.status != sigil_kernel::OrchestrationEvalRouteStatus::Qualified)
        .map(|gate| {
            format!(
                "{}={:?} ({})",
                gate.identity_digest,
                gate.status,
                gate.reasons.join("; ")
            )
        })
        .collect::<Vec<_>>();
    if !rejected.is_empty() {
        anyhow::bail!(
            "orchestration eval acceptance failed: {}",
            rejected.join(", ")
        );
    }
    Ok(())
}

fn resolve_model_eval_fixture_roots(launch_cwd: &Path, cases: &[String]) -> Result<Vec<PathBuf>> {
    if cases.is_empty() {
        anyhow::bail!("model eval requires at least one --case");
    }
    let fixture_root = launch_cwd.join("dev/evals/model-fixtures");
    let mut fixture_roots = Vec::new();
    for case in cases {
        if case == "orchestration-v1" {
            fixture_roots.extend(resolve_frozen_orchestration_fixture_roots(&fixture_root)?);
            continue;
        }
        let relative = Path::new(case);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            anyhow::bail!("model eval case must be a safe relative fixture path: {case}");
        }
        fixture_roots.push(fixture_root.join(relative));
    }
    Ok(fixture_roots)
}

fn resolve_frozen_orchestration_fixture_roots(fixture_root: &Path) -> Result<Vec<PathBuf>> {
    let corpus_root = fixture_root.join("orchestration-v1");
    let mut fixture_roots = Vec::new();
    for case_class in ["negative", "positive"] {
        let class_root = corpus_root.join(case_class);
        let metadata = std::fs::symlink_metadata(&class_root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            anyhow::bail!(
                "orchestration-v1 case class must be a regular directory: {}",
                class_root.display()
            );
        }
        for entry in std::fs::read_dir(&class_root)? {
            let entry = entry?;
            let metadata = entry.file_type()?;
            if metadata.is_symlink() || !metadata.is_dir() {
                anyhow::bail!(
                    "orchestration-v1 case must be a regular directory: {}",
                    entry.path().display()
                );
            }
            fixture_roots.push(entry.path());
        }
    }
    fixture_roots.sort();
    if fixture_roots.len() != 30 {
        anyhow::bail!(
            "orchestration-v1 corpus must contain exactly 30 cases, found {}",
            fixture_roots.len()
        );
    }
    Ok(fixture_roots)
}

fn parse_model_eval_cost_microusd(raw: &str) -> Result<u64> {
    let value = raw
        .parse::<f64>()
        .map_err(|_| anyhow::anyhow!("--max-cost-usd must be a positive decimal"))?;
    if !value.is_finite() || value <= 0.0 || value > u64::MAX as f64 / 1_000_000.0 {
        anyhow::bail!("--max-cost-usd is outside the supported positive range");
    }
    Ok((value * 1_000_000.0).ceil() as u64)
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

fn render_version(info: BuildInfo) -> String {
    format!(
        "sigil {}\ncommit: {}\ntarget: {}\nprofile: {}\n",
        info.version, info.git_hash, info.target, info.profile
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

fn load_serve_root_config(config_path: &Path) -> Result<RootConfig> {
    match RootConfig::load(config_path) {
        Ok(config) => Ok(config),
        Err(_) if !config_path.exists() => {
            Ok(sigil_runtime::provider_connections::default_setup_root_config())
        }
        Err(error) => Err(error),
    }
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
    let root_config = load_serve_root_config(config_path)?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    // The durable HTTP journals encode the exact machine protocol. A breaking
    // protocol revision must use a fresh state root so an unreleased older
    // command envelope cannot make the new server fail closed at startup.
    let server_root = paths.workspace_state_root.join(HTTP_SERVER_STATE_DIR);
    let protocol_journal = std::sync::Arc::new(HttpDurableProtocolJournal::open(
        server_root.join("protocol-events.json"),
        4_096,
    )?);
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
    let driver = std::sync::Arc::new(HttpProductionRunDriver::new(
        HttpProductionRunDriverOptions::new(config_path, launch_cwd)
            .with_session_lifecycle(lifecycle.clone()),
        std::sync::Arc::clone(&disclosure_journal),
        std::sync::Arc::clone(&event_bus),
        tokio::runtime::Handle::current(),
    )?);
    let registry = driver.build_registry(command_store)?;
    let session_catalog = std::sync::Arc::new(SessionCatalogProjectionService::new(
        lifecycle,
        &paths.session_catalog_db,
    ));
    let warm_catalog = std::sync::Arc::clone(&session_catalog);
    let catalog_ready = tokio::task::spawn_blocking(move || warm_catalog.reconcile())
        .await
        .is_ok_and(|result| result.is_ok());
    if !catalog_ready {
        eprintln!(
            "warning: historical session catalog is unavailable; catalog requests will return 503"
        );
    }
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
    .with_support_context(HttpSupportContext::new(
        config_path,
        launch_cwd,
        BuildInfo::current().into(),
    ));
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

fn cli_application_run_request(
    config_path: &Path,
    launch_cwd: &Path,
    prompt: String,
    connection: Option<&str>,
    model: Option<&str>,
) -> std::result::Result<ApplicationRunRequest, ApplicationRunPrepareError> {
    let mut request = ApplicationRunRequest::non_interactive(
        config_path,
        launch_cwd,
        prompt,
        uuid::Uuid::new_v4().to_string(),
    );
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

async fn run_command(
    config_path: &Path,
    launch_cwd: &Path,
    prompt: String,
    connection: Option<&str>,
    model: Option<&str>,
) -> Result<()> {
    let disclosure_presenter: std::sync::Arc<dyn sigil_kernel::EgressDisclosurePresenter> =
        std::sync::Arc::new(crate::egress_disclosure::CliEgressDisclosurePresenter::stderr());
    let services = ApplicationRunServices::new(disclosure_presenter);
    let request = cli_application_run_request(config_path, launch_cwd, prompt, connection, model)?;
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
    let services = ApplicationRunServices::new(disclosure_presenter);
    let mut cancellation = Box::pin(cancellation);
    let request =
        match cli_application_run_request(config_path, launch_cwd, prompt, connection, model) {
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
                        session_log_path,
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
            let status = match run.terminal_status {
                ApplicationRunTerminalStatus::Succeeded => MachineRunStatus::Succeeded,
                ApplicationRunTerminalStatus::Interrupted
                | ApplicationRunTerminalStatus::Blocked => MachineRunStatus::Failed,
            };
            let result = MachineRunResult {
                session_id: run.session_id,
                run_id: run.run_id,
                status,
                final_text: run.agent_output.result.final_text,
                session_log_path,
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
    let code = match error.class() {
        ApplicationRunPrepareErrorClass::InvalidInvocation => MachineErrorCode::InvalidInvocation,
        ApplicationRunPrepareErrorClass::Configuration => MachineErrorCode::ConfigurationInvalid,
        ApplicationRunPrepareErrorClass::ModelRouteNotConfigured => {
            MachineErrorCode::ModelRouteNotConfigured
        }
        ApplicationRunPrepareErrorClass::Execution => MachineErrorCode::ExecutionFailed,
        ApplicationRunPrepareErrorClass::Internal => MachineErrorCode::Internal,
    };
    MachineError::new(code, error.to_string(), false)
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
        PublicRunEventKind::Usage { usage } => render_stream_event(StreamRenderEvent::Usage(usage)),
        PublicRunEventKind::Notice { message } => RenderedOutput {
            stderr: format!("[notice] {message}\n"),
            ..RenderedOutput::default()
        },
        PublicRunEventKind::RunStarted { .. }
        | PublicRunEventKind::TaskRunStarted { .. }
        | PublicRunEventKind::RunFinished { .. }
        | PublicRunEventKind::TaskRunFinished { .. }
        | PublicRunEventKind::TaskRoutingChanged { .. }
        | PublicRunEventKind::TaskPhaseChanged { .. }
        | PublicRunEventKind::TaskPlanUpdated { .. }
        | PublicRunEventKind::TaskBatchChanged { .. }
        | PublicRunEventKind::TaskStepChanged { .. }
        | PublicRunEventKind::IntegrationLaneChanged { .. }
        | PublicRunEventKind::RunFailed { .. }
        | PublicRunEventKind::RunCancelled
        | PublicRunEventKind::ContinuationState { .. }
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
