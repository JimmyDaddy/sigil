//! RFC-0071 R71.4: release-owner frozen orchestration route contract builder (nonshipping).

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    /// Launch cwd (defaults to current directory).
    #[arg(long, default_value = ".")]
    launch_cwd: PathBuf,
    /// Provider system fingerprint captured by the release-owner live probe.
    #[arg(long)]
    provider_system_fingerprint: String,
    /// Candidate build git hash (must match runtime build metadata).
    #[arg(long = "sigil-commit")]
    sigil_commit: String,
    #[arg(long = "case", default_value = "orchestration-v1")]
    cases: Vec<String>,
    #[arg(long)]
    output: PathBuf,
    /// Configuration path.
    #[arg(long = "config", default_value = "sigil.toml")]
    config_path: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let launch_cwd = std::path::absolute(&args.launch_cwd)?;
    let fixture_roots =
        sigil_release_tools::resolve_model_eval_fixture_roots(&launch_cwd, &args.cases)?;
    let output = if args.output.is_absolute() {
        args.output.clone()
    } else {
        launch_cwd.join(&args.output)
    };
    let contract = sigil_runtime::model_eval::build_model_eval_orchestration_route_contract(
        &sigil_runtime::model_eval::ModelEvalRouteContractBuildRequest {
            config_path: args.config_path,
            fixture_roots,
            provider_system_fingerprint: args.provider_system_fingerprint,
        },
    )?;
    if contract.sigil_commit != args.sigil_commit {
        anyhow::bail!(
            "candidate CLI and runtime build metadata disagree; rebuild the frozen binary"
        );
    }
    sigil_runtime::model_eval::write_model_eval_orchestration_route_contract(&contract, &output)?;
    println!("wrote {}", output.display());
    Ok(())
}
