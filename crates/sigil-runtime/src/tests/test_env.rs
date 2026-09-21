use std::{
    ffi::{OsStr, OsString},
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
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvScope {
    pub(crate) fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: runtime tests serialize environment mutations through `lock`.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        // SAFETY: the guard that owns this scope still serializes environment access during drop.
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}
