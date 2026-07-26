mod capabilities;
mod catalog;
mod client;
mod config;
mod errors;
mod mapper;
mod models;
mod provider;
mod request;
mod stream;

pub use capabilities::openai_compatible_capabilities;
pub use catalog::{OpenAiCompatibleCatalogModel, parse_openai_compatible_model_list};
pub use config::{
    OPENAI_COMPATIBLE_API_KEY_ENV, OPENAI_COMPATIBLE_BASE_URL_ENV, OpenAiCompatibleProviderConfig,
};
pub use provider::OpenAiCompatibleProvider;

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
