//! A finite synchronous operation with an explicit join owner. This utility does not grant
//! process/resource authority; callers must move their existing admitted owners into the work.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

/// Failure to start or join an owned blocking operation. Work-specific errors remain in `T`.
#[derive(Debug, thiserror::Error)]
pub enum OwnedBlockingWorkError {
    #[error("owned blocking operation could not start")]
    Spawn,
    #[error("owned blocking operation panicked or could not be joined")]
    Join,
}

enum Worker<T> {
    Tokio(tokio::task::JoinHandle<T>),
    Native {
        handle: std::thread::JoinHandle<T>,
        done: tokio::sync::oneshot::Receiver<()>,
    },
}

/// Retains the join handle while async callers wait. Dropping the future is an exceptional
/// synchronous join, never cancellation of the underlying operation. Work must be finite and
/// must not wait for the calling async task/runtime to make progress.
pub struct OwnedBlockingWork<T> {
    worker: Option<Worker<T>>,
}

impl<T: Send + 'static> OwnedBlockingWork<T> {
    /// Starts finite synchronous work on Tokio's blocking pool, or on an owned native thread
    /// for non-Tokio callers. The label is host-owned and must not contain user data.
    ///
    /// # Errors
    /// Returns `Spawn` when the native thread could not be created.
    pub fn spawn(
        label: &'static str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<Self, OwnedBlockingWorkError> {
        let worker = if tokio::runtime::Handle::try_current().is_ok() {
            Worker::Tokio(tokio::task::spawn_blocking(work))
        } else {
            let (done, completion) = tokio::sync::oneshot::channel();
            let handle = std::thread::Builder::new()
                .name(label.to_owned())
                .spawn(move || {
                    let result = work();
                    let _ = done.send(());
                    result
                })
                .map_err(|_| OwnedBlockingWorkError::Spawn)?;
            Worker::Native {
                handle,
                done: completion,
            }
        };
        Ok(Self {
            worker: Some(worker),
        })
    }
}

impl<T> Future for OwnedBlockingWork<T> {
    type Output = Result<T, OwnedBlockingWorkError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(worker) = self.worker.as_mut() else {
            return Poll::Ready(Err(OwnedBlockingWorkError::Join));
        };
        match worker {
            Worker::Tokio(handle) => match Pin::new(handle).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(result) => {
                    self.worker.take();
                    Poll::Ready(result.map_err(|_| OwnedBlockingWorkError::Join))
                }
            },
            Worker::Native { done, .. } => {
                if Pin::new(done).poll(cx).is_pending() {
                    return Poll::Pending;
                }
                let Some(Worker::Native { handle, .. }) = self.worker.take() else {
                    return Poll::Ready(Err(OwnedBlockingWorkError::Join));
                };
                Poll::Ready(handle.join().map_err(|_| OwnedBlockingWorkError::Join))
            }
        }
    }
}

impl<T> Drop for OwnedBlockingWork<T> {
    fn drop(&mut self) {
        match self.worker.take() {
            Some(Worker::Native { handle, .. }) => {
                let _ = handle.join();
            }
            Some(Worker::Tokio(mut handle)) => {
                // Only the blocking-pool job is awaited here. Its contract forbids awaiting
                // this async caller, so joining needs no progress from the Tokio reactor.
                struct JoinWake(std::thread::Thread);
                impl Wake for JoinWake {
                    fn wake(self: Arc<Self>) {
                        self.0.unpark();
                    }
                }
                let waker = Waker::from(Arc::new(JoinWake(std::thread::current())));
                let mut context = Context::from_waker(&waker);
                while Pin::new(&mut handle).poll(&mut context).is_pending() {
                    std::thread::park();
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
#[path = "tests/owned_blocking_tests.rs"]
mod tests;
