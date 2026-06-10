mod capabilities;
mod client;
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

pub use config::{
    DeepSeekProviderConfig, DeepSeekProviderProfile, DeepSeekProviderQuirkProfile,
    LEGACY_DEEPSEEK_API_KEY_ENV, LEGACY_TERMQUILL_DEEPSEEK_API_KEY_ENV, StrictToolsMode,
    TERMQUILL_ANTHROPIC_BASE_URL_ENV, TERMQUILL_API_KEY_ENV, TERMQUILL_BASE_URL_ENV,
    TERMQUILL_BETA_BASE_URL_ENV, TERMQUILL_FIM_MODEL_ENV, TERMQUILL_MODEL_ENV,
    TERMQUILL_REQUEST_TIMEOUT_SECS_ENV, TERMQUILL_STRICT_TOOLS_MODE_ENV,
    TERMQUILL_USER_ID_STRATEGY_ENV,
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
