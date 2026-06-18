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

pub use capabilities::deepseek_capabilities;
pub use config::{
    DeepSeekProviderConfig, DeepSeekProviderProfile, DeepSeekProviderQuirkProfile,
    LEGACY_DEEPSEEK_API_KEY_ENV, SIGIL_ANTHROPIC_BASE_URL_ENV, SIGIL_API_KEY_ENV,
    SIGIL_BASE_URL_ENV, SIGIL_BETA_BASE_URL_ENV, SIGIL_FIM_MODEL_ENV, SIGIL_MODEL_ENV,
    SIGIL_REQUEST_TIMEOUT_SECS_ENV, SIGIL_STRICT_TOOLS_MODE_ENV, SIGIL_USER_ID_STRATEGY_ENV,
    StrictToolsMode,
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
