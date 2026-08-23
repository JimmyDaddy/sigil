//! RFC-0071 R71.4: release-owner rollout manifest builder (nonshipping).

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = ".")]
    launch_cwd: PathBuf,
    #[arg(long)]
    report: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let launch_cwd = std::path::absolute(&args.launch_cwd)?;
    let report = if args.report.is_absolute() {
        args.report.clone()
    } else {
        launch_cwd.join(&args.report)
    };
    let output = if args.output.is_absolute() {
        args.output.clone()
    } else {
        launch_cwd.join(&args.output)
    };
    let report = sigil_runtime::load_orchestration_eval_report_manifest(&report)?;
    let manifest = sigil_runtime::build_orchestration_rollout_manifest(&report)?;
    sigil_runtime::write_orchestration_rollout_manifest(&manifest, &output)?;
    println!("wrote {}", output.display());
    Ok(())
}
