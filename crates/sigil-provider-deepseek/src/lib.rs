mod capabilities;
mod client;
mod compaction_token_profile;
mod config;
mod endpoint;
mod errors;
mod fim;
mod mapper;
mod models;
mod prefix;
mod pricing;
mod provider;
mod reasoning;
mod request;
mod response;
mod retry;
mod stream;
mod tools;

pub use capabilities::deepseek_capabilities;
pub use compaction_token_profile::{
    DEFAULT_DEEPSEEK_V4_FLASH_ENCODER_SHA256, DEFAULT_DEEPSEEK_V4_FLASH_HOSTED_SYSTEM_FINGERPRINT,
    DEFAULT_DEEPSEEK_V4_FLASH_MODEL, DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS,
    DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_SAFETY_BUFFER_TOKENS,
    DEFAULT_DEEPSEEK_V4_FLASH_REVISION, DEFAULT_DEEPSEEK_V4_FLASH_TOKENIZER_SHA256,
    DeepSeekV4FlashPortableTargetAdmission, DeepSeekV4FlashTokenCounter,
    default_deepseek_v4_flash_portable_target_budget,
    default_deepseek_v4_flash_portable_target_output_tokens,
    default_deepseek_v4_flash_token_binding, default_deepseek_v4_flash_tokenizer_cache_path,
    default_deepseek_v4_flash_tokenizer_url, download_default_deepseek_v4_flash_tokenizer,
};
pub use config::{
    DeepSeekProviderConfig, DeepSeekProviderProfile, DeepSeekProviderQuirkProfile,
    SIGIL_ANTHROPIC_BASE_URL_ENV, SIGIL_API_KEY_ENV, SIGIL_BASE_URL_ENV, SIGIL_BETA_BASE_URL_ENV,
    SIGIL_FIM_MODEL_ENV, SIGIL_STRICT_TOOLS_MODE_ENV, SIGIL_USER_ID_STRATEGY_ENV, StrictToolsMode,
};
pub use fim::DeepSeekFimCompletionRequest;
pub use prefix::DeepSeekPrefixCompletionRequest;
pub use pricing::context_window_tokens as deepseek_context_window_tokens;
pub use provider::DeepSeekProvider;

#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    pub(crate) fn lock() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }
}
