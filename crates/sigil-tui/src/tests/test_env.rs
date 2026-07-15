use std::{
    ffi::OsString,
    sync::{Mutex, MutexGuard, OnceLock},
};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) fn lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) struct EnvScope {
    name: &'static str,
    saved: Option<OsString>,
}

impl EnvScope {
    pub(crate) fn set(name: &'static str, value: &str) -> Self {
        let saved = std::env::var_os(name);
        // SAFETY: TUI tests that mutate provider environment variables serialize through the
        // shared test environment lock.
        unsafe { std::env::set_var(name, value) };
        Self { name, saved }
    }

    pub(crate) fn unset(name: &'static str) -> Self {
        let saved = std::env::var_os(name);
        // SAFETY: TUI tests that mutate provider environment variables serialize through the
        // shared test environment lock.
        unsafe { std::env::remove_var(name) };
        Self { name, saved }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        match self.saved.take() {
            Some(value) => {
                // SAFETY: the shared lock remains held until this scope is dropped.
                unsafe { std::env::set_var(self.name, value) };
            }
            None => {
                // SAFETY: the shared lock remains held until this scope is dropped.
                unsafe { std::env::remove_var(self.name) };
            }
        }
    }
}
