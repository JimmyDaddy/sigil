//! Native ownership of the renderer's cancellable display/transcript observations.

use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::watch;

const MAX_HISTORY_QUERIES: usize = 32;
const UNUSED_HISTORY_QUERY_LIFETIME: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum HistoryQueryError {
    #[error("history query was cancelled or is no longer registered")]
    Cancelled,
    #[error("history query belongs to another workspace or session")]
    BindingMismatch,
    #[error("history query is already in progress")]
    AlreadyClaimed,
    #[error("too many history queries are pending")]
    Capacity,
    #[error("history query owner is unavailable")]
    Unavailable,
}

#[derive(Default)]
struct Queries {
    next_id: u64,
    stopping: bool,
    pending: BTreeMap<String, Ticket>,
}

struct Ticket {
    workspace: String,
    session: String,
    claimed: bool,
    prepared_at: Instant,
    cancellation: watch::Sender<bool>,
}

/// At most 32 pre-registered history reads. A ticket grants cancellation only for its exact
/// binding; it conveys no path, HTTP transport, session writer or run authority.
#[derive(Clone, Default)]
pub(crate) struct DesktopHistoryQueryOwner(Arc<Mutex<Queries>>);

impl DesktopHistoryQueryOwner {
    pub(crate) fn prepare(
        &self,
        workspace: &str,
        session: &str,
    ) -> Result<String, HistoryQueryError> {
        let mut state = self.0.lock().map_err(|_| HistoryQueryError::Unavailable)?;
        if state.stopping {
            return Err(HistoryQueryError::Cancelled);
        }
        expire_unused(&mut state, Instant::now());
        if state.pending.len() >= MAX_HISTORY_QUERIES {
            return Err(HistoryQueryError::Capacity);
        }
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or(HistoryQueryError::Unavailable)?;
        // IDs never repeat within this owner, including after cancellation or workspace close.
        let id = format!("history-query-{:016x}", state.next_id);
        let (cancellation, _) = watch::channel(false);
        state.pending.insert(
            id.clone(),
            Ticket {
                workspace: workspace.to_owned(),
                session: session.to_owned(),
                claimed: false,
                prepared_at: Instant::now(),
                cancellation,
            },
        );
        Ok(id)
    }

    pub(crate) fn claim(
        &self,
        workspace: &str,
        session: &str,
        id: &str,
    ) -> Result<HistoryQueryLease, HistoryQueryError> {
        let mut state = self.0.lock().map_err(|_| HistoryQueryError::Unavailable)?;
        expire_unused(&mut state, Instant::now());
        let ticket = state
            .pending
            .get_mut(id)
            .ok_or(HistoryQueryError::Cancelled)?;
        validate_binding(ticket, workspace, session)?;
        if ticket.claimed {
            return Err(HistoryQueryError::AlreadyClaimed);
        }
        ticket.claimed = true;
        Ok(HistoryQueryLease {
            owner: self.clone(),
            id: id.to_owned(),
            cancellation: ticket.cancellation.subscribe(),
        })
    }

    pub(crate) fn cancel(
        &self,
        workspace: &str,
        session: &str,
        id: &str,
    ) -> Result<(), HistoryQueryError> {
        let mut state = self.0.lock().map_err(|_| HistoryQueryError::Unavailable)?;
        let Some(ticket) = state.pending.get(id) else {
            return Ok(());
        };
        validate_binding(ticket, workspace, session)?;
        ticket.cancellation.send_replace(true);
        // Claimed reads continue occupying capacity until their real future has been dropped.
        if !ticket.claimed {
            state.pending.remove(id);
        }
        Ok(())
    }

    pub(crate) fn cancel_workspace(&self, workspace: &str) -> Result<(), HistoryQueryError> {
        let mut state = self.0.lock().map_err(|_| HistoryQueryError::Unavailable)?;
        state.pending.retain(|_, ticket| {
            if ticket.workspace != workspace {
                return true;
            }
            ticket.cancellation.send_replace(true);
            ticket.claimed
        });
        Ok(())
    }

    pub(crate) fn stop(&self) {
        let Ok(mut state) = self.0.lock() else {
            return;
        };
        state.stopping = true;
        state.pending.retain(|_, ticket| {
            ticket.cancellation.send_replace(true);
            ticket.claimed
        });
    }
}

fn expire_unused(state: &mut Queries, now: Instant) {
    // A renderer reload can lose prepare's reply. Retire only unclaimed tickets lazily;
    // long-running cold reads remain cancellable without inheriting this registration timeout.
    state.pending.retain(|_, ticket| {
        ticket.claimed
            || now.saturating_duration_since(ticket.prepared_at) < UNUSED_HISTORY_QUERY_LIFETIME
    });
}

fn validate_binding(
    ticket: &Ticket,
    workspace: &str,
    session: &str,
) -> Result<(), HistoryQueryError> {
    if ticket.workspace != workspace || ticket.session != session {
        return Err(HistoryQueryError::BindingMismatch);
    }
    Ok(())
}

/// Keeps cancellation registered through validation, manager acquisition and the real HTTP read.
pub(crate) struct HistoryQueryLease {
    owner: DesktopHistoryQueryOwner,
    id: String,
    cancellation: watch::Receiver<bool>,
}

impl HistoryQueryLease {
    pub(crate) async fn run<F: Future>(mut self, work: F) -> Result<F::Output, HistoryQueryError> {
        let cancelled = *self.cancellation.borrow();
        if cancelled {
            return Err(HistoryQueryError::Cancelled);
        }
        tokio::select! {
            biased;
            _ = self.cancellation.changed() => Err(HistoryQueryError::Cancelled),
            result = work => Ok(result),
        }
    }
}

impl Drop for HistoryQueryLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.owner.0.lock() {
            state.pending.remove(&self.id);
        }
    }
}

#[cfg(test)]
#[path = "tests/history_queries_tests.rs"]
mod tests;
