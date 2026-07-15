use std::{
    ffi::OsString,
    sync::{Mutex, MutexGuard, OnceLock},
};

use crate::{
    SIGIL_ANTHROPIC_BASE_URL_ENV, SIGIL_API_KEY_ENV, SIGIL_BASE_URL_ENV, SIGIL_BETA_BASE_URL_ENV,
    SIGIL_FIM_MODEL_ENV, SIGIL_STRICT_TOOLS_MODE_ENV, SIGIL_USER_ID_STRATEGY_ENV,
};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) fn lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn with_clean_provider_env<T>(run: impl FnOnce() -> T) -> T {
    const NAMES: [&str; 7] = [
        SIGIL_API_KEY_ENV,
        SIGIL_BASE_URL_ENV,
        SIGIL_BETA_BASE_URL_ENV,
        SIGIL_ANTHROPIC_BASE_URL_ENV,
        SIGIL_USER_ID_STRATEGY_ENV,
        SIGIL_FIM_MODEL_ENV,
        SIGIL_STRICT_TOOLS_MODE_ENV,
    ];

    let _lock = lock();
    let previous = NAMES.map(std::env::var_os);
    for name in NAMES {
        // SAFETY: all tests that mutate this provider's environment use ENV_LOCK.
        unsafe { std::env::remove_var(name) };
    }
    let _restore = ProviderEnvRestore { previous };
    run()
}

struct ProviderEnvRestore {
    previous: [Option<OsString>; 7],
}

impl Drop for ProviderEnvRestore {
    fn drop(&mut self) {
        const NAMES: [&str; 7] = [
            SIGIL_API_KEY_ENV,
            SIGIL_BASE_URL_ENV,
            SIGIL_BETA_BASE_URL_ENV,
            SIGIL_ANTHROPIC_BASE_URL_ENV,
            SIGIL_USER_ID_STRATEGY_ENV,
            SIGIL_FIM_MODEL_ENV,
            SIGIL_STRICT_TOOLS_MODE_ENV,
        ];
        for (name, value) in NAMES.into_iter().zip(&self.previous) {
            match value {
                Some(value) => {
                    // SAFETY: the matching ENV_LOCK remains held until this guard drops.
                    unsafe { std::env::set_var(name, value) };
                }
                None => {
                    // SAFETY: the matching ENV_LOCK remains held until this guard drops.
                    unsafe { std::env::remove_var(name) };
                }
            }
        }
    }
}
