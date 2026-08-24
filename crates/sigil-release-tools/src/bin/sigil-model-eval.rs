//! RFC-0071 R71.4: release-owner model eval campaign runner (nonshipping).

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = ".")]
    launch_cwd: PathBuf,
    #[arg(long, required = true)]
    case: Vec<String>,
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
    #[arg(long = "config", default_value = "sigil.toml")]
    config_path: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let launch_cwd = std::path::absolute(&args.launch_cwd)?;
    let fixture_roots =
        sigil_release_tools::resolve_model_eval_fixture_roots(&launch_cwd, &args.case)?;
    let output_dir = if args.output_dir.is_absolute() {
        args.output_dir.clone()
    } else {
        launch_cwd.join(&args.output_dir)
    };
    if let Some(parent) = output_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let release_output_owner = std::sync::Arc::new(sigil_release_tools::ReleaseOutputOwnerV1::new(
        output_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("model eval output has no parent"))?,
    ));
    let orchestration_route_contract = args
        .orchestration_route_contract
        .map(|path| {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                launch_cwd.join(&path)
            };
            load_route_contract(&path)
        })
        .transpose()?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let disclosure_presenter: std::sync::Arc<dyn sigil_kernel::EgressDisclosurePresenter> =
                std::sync::Arc::new(SilentDisclosurePresenter);
            let services =
                sigil_runtime::application_run::ApplicationRunServices::new(disclosure_presenter);
            let campaign = sigil_runtime::model_eval::run_model_eval_campaign(
                sigil_runtime::model_eval::ModelEvalCampaignRequest {
                    config_path: args.config_path,
                    fixture_roots,
                    orchestration_route_contract,
                    repetitions: args.repetitions,
                    max_cost_microusd: sigil_release_tools::parse_model_eval_cost_microusd(
                        &args.max_cost_usd,
                    )?,
                    campaign_timeout: std::time::Duration::from_secs(args.timeout_secs),
                    output_dir,
                    release_output_owner: Some(release_output_owner),
                },
                &services,
            )
            .await?;
            let manifest_path = campaign.output_dir.join("manifest.json");
            let manifest: sigil_kernel::ModelEvalReportManifestV3 =
                serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
            let orchestration_manifest = if campaign.orchestration_route_contract.is_some() {
                let orchestration_manifest_path =
                    campaign.output_dir.join("orchestration/manifest.json");
                Some(serde_json::from_slice::<
                    sigil_kernel::OrchestrationEvalReportManifestV1,
                >(&std::fs::read(
                    orchestration_manifest_path,
                )?)?)
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
            sigil_release_tools::validate_model_eval_manifest(&manifest)?;
            if let Some(manifest) = &orchestration_manifest {
                sigil_release_tools::validate_orchestration_eval_manifest(manifest)?;
            }
            Ok::<(), anyhow::Error>(())
        })
}

fn load_route_contract(
    path: &std::path::Path,
) -> anyhow::Result<sigil_runtime::model_eval::ModelEvalOrchestrationRouteContractV1> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 64 * 1024 {
        anyhow::bail!("orchestration route contract must be a regular file no larger than 64 KiB");
    }
    let bytes = std::fs::read(path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow::anyhow!("orchestration route contract is not UTF-8"))?;
    toml::from_str(text).map_err(|_| anyhow::anyhow!("orchestration route contract is invalid"))
}

/// Release-tool disclosure presenter: campaign runs are release-owned; disclosure is sent to
/// standard error with a stable sink fingerprint (never machine-readable stdout).
struct SilentDisclosurePresenter;

#[async_trait::async_trait]
impl sigil_kernel::EgressDisclosurePresenter for SilentDisclosurePresenter {
    async fn present(
        &self,
        disclosure: sigil_kernel::PreEgressDisclosure,
    ) -> Result<
        sigil_kernel::DisclosurePresentationReceipt,
        sigil_kernel::DisclosurePresentationError,
    > {
        eprintln!(
            "[sigil release-tools disclosure] {}",
            disclosure.display_name()
        );
        disclosure.presentation_receipt("release-tools-stderr-v1")
    }
}
