mod capabilities;
mod client;
mod config;
mod errors;
mod hosted_search;
mod mapper;
mod models;
mod provider;
mod request;
mod stream;

pub use capabilities::anthropic_capabilities;
pub use config::{
    AnthropicProviderConfig, SIGIL_ANTHROPIC_API_KEY_ENV, SIGIL_ANTHROPIC_BASE_URL_ENV,
    SIGIL_ANTHROPIC_MAX_TOKENS_ENV, SIGIL_ANTHROPIC_VERSION_ENV,
};
pub use provider::AnthropicProvider;

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
