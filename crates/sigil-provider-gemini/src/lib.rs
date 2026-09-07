mod capabilities;
mod catalog;
mod config;
mod errors;
mod hosted_search;
mod mapper;
mod models;
mod provider;
mod request;
mod stream;

pub use capabilities::gemini_capabilities;
pub use catalog::{
    BUNDLED_GEMINI_MODELS, GeminiCatalogModel, GeminiCatalogPage, gemini_maximum_output_tokens,
    parse_gemini_model_list,
};
pub use config::{
    GEMINI_API_KEY_ENV, GOOGLE_API_KEY_ENV, GeminiProviderConfig, SIGIL_GEMINI_API_KEY_ENV,
    SIGIL_GEMINI_API_KEY_ENV_NAMES, SIGIL_GEMINI_BASE_URL_ENV,
};
pub use provider::GeminiProvider;

#[cfg(test)]
#[path = "tests/catalog_tests.rs"]
mod catalog_tests;
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
