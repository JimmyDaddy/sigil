use std::{path::Path, pin::Pin};

use anyhow::Result;
use futures::Stream;
use sigil_kernel::{InteractionMode, ProviderChunk, RootConfig, resolve_workspace_root};
use sigil_provider_deepseek::{
    DeepSeekFimCompletionRequest, DeepSeekPrefixCompletionRequest, DeepSeekProvider,
};

use crate::{build_run_options, load_deepseek_config};

pub type ProviderDebugStream = Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>;

/// Hidden developer-only DeepSeek prefix completion request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekPrefixDebugRequest {
    pub prompt: String,
    pub assistant_prefix: String,
    pub stop: Vec<String>,
    pub model: Option<String>,
}

/// Hidden developer-only DeepSeek fill-in-the-middle request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekFimDebugRequest {
    pub prompt: String,
    pub suffix: String,
    pub stop: Vec<String>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
}

/// Starts the hidden DeepSeek prefix-completion debug flow.
///
/// This is intentionally a runtime adapter rather than a product command surface:
/// the `sigil` binary can keep its hidden developer command without directly
/// depending on provider crates or leaking provider-specific request types.
///
/// # Errors
///
/// Returns an error when the DeepSeek provider config is missing, invalid, or
/// when the provider cannot establish the debug completion stream.
pub async fn stream_deepseek_prefix_debug(
    root_config: &RootConfig,
    config_path: &Path,
    launch_cwd: &Path,
    request: DeepSeekPrefixDebugRequest,
) -> Result<ProviderDebugStream> {
    let provider = load_deepseek_debug_provider(root_config)?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let traffic_partition_key =
        build_run_options(root_config, workspace_root, InteractionMode::Headless)
            .traffic_partition_key;
    provider
        .stream_prefix_completion(DeepSeekPrefixCompletionRequest {
            model: request.model,
            prompt: request.prompt,
            assistant_prefix: request.assistant_prefix,
            stop: request.stop,
            reasoning_effort: None,
            traffic_partition_key,
        })
        .await
}

/// Starts the hidden DeepSeek fill-in-the-middle debug flow.
///
/// # Errors
///
/// Returns an error when the DeepSeek provider config is missing, invalid, or
/// when the provider cannot establish the debug completion stream.
pub async fn stream_deepseek_fim_debug(
    root_config: &RootConfig,
    request: DeepSeekFimDebugRequest,
) -> Result<ProviderDebugStream> {
    let provider = load_deepseek_debug_provider(root_config)?;
    provider
        .stream_fim_completion(DeepSeekFimCompletionRequest {
            model: request.model,
            prompt: request.prompt,
            suffix: request.suffix,
            max_tokens: request.max_tokens,
            stop: request.stop,
        })
        .await
}

fn load_deepseek_debug_provider(root_config: &RootConfig) -> Result<DeepSeekProvider> {
    DeepSeekProvider::new(load_deepseek_config(root_config)?)
}

#[cfg(test)]
#[path = "tests/provider_debug_tests.rs"]
mod tests;
