//! OpenAI Responses API provider.
//!
//! This crate intentionally does not share request or stream DTOs with the OpenAI-compatible
//! Chat Completions provider. The Responses API has a different canonical input/output-item
//! protocol, which must remain provider-local so later stateless compaction can preserve opaque
//! response items without changing Chat Completions semantics.

mod capabilities;
mod client;
mod config;
mod errors;
mod mapper;
mod models;
mod provider;
mod reasoning_effort;
mod request;
mod stream;

pub use capabilities::openai_responses_capabilities;
pub use config::{
    OPENAI_RESPONSES_API_KEY_ENV, OPENAI_RESPONSES_BASE_URL_ENV, OpenAiResponsesProviderConfig,
};
pub use models::OpenAiResponsesCompactedWindow;
pub use provider::{
    OPENAI_RESPONSES_PORTABLE_TARGET_CONTEXT_WINDOW_TOKENS, OPENAI_RESPONSES_PORTABLE_TARGET_MODEL,
    OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS, OpenAiResponsesProvider,
};
pub use reasoning_effort::openai_responses_reasoning_efforts;
pub use request::OPENAI_RESPONSES_OUTPUT_ITEMS_STATE_KIND;

#[cfg(test)]
#[path = "tests/reasoning_effort_tests.rs"]
mod reasoning_effort_tests;

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
