use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Weak},
};

use sigil_mcp::{
    McpOAuthCredentialError, McpOAuthCredentialRecord, McpOAuthCredentialScope,
    McpOAuthCredentialSnapshot, McpOAuthCredentialStatus, McpOAuthCredentialStore,
    McpOAuthHttpExecutor, McpOAuthRevocationOutcome, SystemMcpOAuthCredentialStore,
    refresh_oauth_credential, revoke_oauth_credential,
};
use tokio::sync::Mutex;

/// Runtime owner for keyring persistence and per-scope single-flight OAuth refresh.
pub struct McpOAuthCredentialManager {
    store: Arc<dyn McpOAuthCredentialStore>,
    refresh_locks: Mutex<BTreeMap<String, Weak<Mutex<()>>>>,
}

impl fmt::Debug for McpOAuthCredentialManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthCredentialManager")
            .finish_non_exhaustive()
    }
}

impl Default for McpOAuthCredentialManager {
    fn default() -> Self {
        Self::system()
    }
}

impl McpOAuthCredentialManager {
    /// Uses the native system credential store. There is deliberately no file fallback.
    #[must_use]
    pub fn system() -> Self {
        Self::new(Arc::new(SystemMcpOAuthCredentialStore))
    }

    /// Builds a manager around a credential store implementation, primarily for conformance tests.
    #[must_use]
    pub fn new(store: Arc<dyn McpOAuthCredentialStore>) -> Self {
        Self {
            store,
            refresh_locks: Mutex::new(BTreeMap::new()),
        }
    }

    /// Persists one complete versioned record before it can be used by MCP requests.
    pub async fn persist(
        &self,
        record: &McpOAuthCredentialRecord,
    ) -> Result<(), McpOAuthCredentialError> {
        self.store.store(record).await
    }

    /// Returns a secret-free status projection; unavailable stores never become `Missing`.
    pub async fn status(
        &self,
        scope: &McpOAuthCredentialScope,
        now_epoch_secs: u64,
    ) -> McpOAuthCredentialStatus {
        match self.store.load(scope).await {
            Ok(Some(record)) => record.status(now_epoch_secs),
            Ok(None) => McpOAuthCredentialStatus::Missing,
            Err(_) => McpOAuthCredentialStatus::Unavailable,
        }
    }

    /// Returns a current bearer snapshot, refreshing once per scope when inside the expiry skew.
    pub async fn bearer_snapshot(
        &self,
        scope: &McpOAuthCredentialScope,
        static_header_fingerprint: &str,
        now_epoch_secs: u64,
        executor: &dyn McpOAuthHttpExecutor,
    ) -> Result<McpOAuthCredentialSnapshot, McpOAuthCredentialError> {
        let record = self
            .store
            .load(scope)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
        if record.status(now_epoch_secs) == McpOAuthCredentialStatus::Present {
            return record.snapshot(static_header_fingerprint, now_epoch_secs);
        }
        self.refresh_and_snapshot(scope, static_header_fingerprint, now_epoch_secs, executor)
            .await
    }

    /// Explicitly refreshes a credential without retrying any MCP request.
    pub async fn refresh_now(
        &self,
        scope: &McpOAuthCredentialScope,
        static_header_fingerprint: &str,
        now_epoch_secs: u64,
        executor: &dyn McpOAuthHttpExecutor,
    ) -> Result<McpOAuthCredentialSnapshot, McpOAuthCredentialError> {
        let observed = self
            .store
            .load(scope)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?
            .generation_id()
            .to_owned();
        let lock = self.refresh_lock(scope).await;
        let _guard = lock.lock().await;
        let record = self
            .store
            .load(scope)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
        if record.generation_id() != observed
            && record.status(now_epoch_secs) == McpOAuthCredentialStatus::Present
        {
            return record.snapshot(static_header_fingerprint, now_epoch_secs);
        }
        self.refresh_owned(record, static_header_fingerprint, now_epoch_secs, executor)
            .await
    }

    /// Marks the current access snapshot unusable after a 401. It does not refresh or retry.
    pub async fn mark_unauthorized(
        &self,
        scope: &McpOAuthCredentialScope,
        now_epoch_secs: u64,
    ) -> Result<(), McpOAuthCredentialError> {
        let Some(record) = self.store.load(scope).await? else {
            return Ok(());
        };
        self.store
            .store(&record.without_access_token(now_epoch_secs))
            .await
    }

    /// Attempts remote revocation without deleting the local keyring record.
    pub async fn revoke(
        &self,
        scope: &McpOAuthCredentialScope,
        executor: &dyn McpOAuthHttpExecutor,
    ) -> Result<McpOAuthRevocationOutcome, McpOAuthCredentialError> {
        let record = self
            .store
            .load(scope)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
        revoke_oauth_credential(executor, &record).await
    }

    /// Deletes only the local system-keyring record and makes no remote-revocation claim.
    pub async fn clear_local(
        &self,
        scope: &McpOAuthCredentialScope,
    ) -> Result<bool, McpOAuthCredentialError> {
        self.store.delete(scope).await
    }

    async fn refresh_and_snapshot(
        &self,
        scope: &McpOAuthCredentialScope,
        static_header_fingerprint: &str,
        now_epoch_secs: u64,
        executor: &dyn McpOAuthHttpExecutor,
    ) -> Result<McpOAuthCredentialSnapshot, McpOAuthCredentialError> {
        let lock = self.refresh_lock(scope).await;
        let _guard = lock.lock().await;
        let record = self
            .store
            .load(scope)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
        if record.status(now_epoch_secs) == McpOAuthCredentialStatus::Present {
            return record.snapshot(static_header_fingerprint, now_epoch_secs);
        }
        self.refresh_owned(record, static_header_fingerprint, now_epoch_secs, executor)
            .await
    }

    async fn refresh_owned(
        &self,
        record: McpOAuthCredentialRecord,
        static_header_fingerprint: &str,
        now_epoch_secs: u64,
        executor: &dyn McpOAuthHttpExecutor,
    ) -> Result<McpOAuthCredentialSnapshot, McpOAuthCredentialError> {
        if !record.can_refresh(now_epoch_secs) {
            return Err(McpOAuthCredentialError::AuthenticationRequired);
        }
        match refresh_oauth_credential(executor, &record).await {
            Ok(token) => {
                let next = record.rotated(&token, now_epoch_secs)?;
                self.store.store(&next).await?;
                next.snapshot(static_header_fingerprint, now_epoch_secs)
            }
            Err(McpOAuthCredentialError::InvalidRefresh) => {
                self.store
                    .store(&record.without_usable_tokens(now_epoch_secs))
                    .await?;
                Err(McpOAuthCredentialError::AuthenticationRequired)
            }
            Err(error) => Err(error),
        }
    }

    async fn refresh_lock(&self, scope: &McpOAuthCredentialScope) -> Arc<Mutex<()>> {
        let mut locks = self.refresh_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(scope.binding_id()).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(scope.binding_id().to_owned(), Arc::downgrade(&lock));
        lock
    }
}

#[cfg(test)]
#[path = "tests/mcp_oauth_tests.rs"]
mod tests;
