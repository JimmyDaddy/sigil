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
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let report = sigil_runtime::load_orchestration_eval_report_manifest(&report)?;
    let manifest = sigil_runtime::build_orchestration_rollout_manifest(&report)?;
    let owner = sigil_release_tools::ReleaseOutputOwnerV1::new(
        output
            .parent()
            .ok_or_else(|| anyhow::anyhow!("rollout manifest output has no parent"))?,
    );
    owner.publish_file(&output, serde_json::to_vec_pretty(&manifest)?.as_slice())?;
    println!("wrote {}", output.display());
    Ok(())
}
