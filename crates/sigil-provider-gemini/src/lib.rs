mod capabilities;
mod client;
mod config;
mod errors;
mod mapper;
mod models;
mod provider;
mod request;
mod stream;

pub use capabilities::gemini_capabilities;
pub use config::{GeminiProviderConfig, SIGIL_GEMINI_API_KEY_ENV, SIGIL_GEMINI_BASE_URL_ENV};
pub use provider::GeminiProvider;

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
