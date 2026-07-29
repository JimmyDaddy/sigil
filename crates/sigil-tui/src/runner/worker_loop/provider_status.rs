use super::*;

pub(in crate::runner) fn drain_provider_status_results(
    provider_status_results: &mut VecDeque<ProviderStatusTaskResult>,
    provider_status_tasks: &mut ProviderStatusTaskManager,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> bool {
    let mut advanced = false;
    while let Some(status_result) = provider_status_results.pop_front() {
        advanced = true;
        match status_result {
            ProviderStatusTaskResult::Balance {
                request_id,
                snapshot,
            } => {
                if provider_status_tasks.accept_balance_result(request_id) {
                    let _ = message_tx.send(WorkerMessage::ProviderBalanceRefreshed {
                        request_id,
                        snapshot,
                    });
                }
            }
            ProviderStatusTaskResult::Models {
                request_id,
                base_url,
                result,
            } => {
                if provider_status_tasks.accept_models_result(request_id) {
                    let _ = message_tx.send(WorkerMessage::ProviderModelsRefreshed {
                        request_id,
                        base_url,
                        result,
                    });
                }
            }
            ProviderStatusTaskResult::ConnectionModels { request_id, result } => {
                if provider_status_tasks.accept_models_result(request_id) {
                    let _ = message_tx.send(WorkerMessage::ConnectionModelsRefreshed { result });
                }
            }
        }
    }
    advanced
}

/// The runtime provider-status task manager currently accepts a standard sender. Its tasks are
/// one-shot, so a blocking one-shot adapter can forward completion into the unified inbox without
/// adding another polled receiver to the worker.
pub(in crate::runner) fn provider_status_result_sender(
    runtime: &tokio::runtime::Runtime,
    event_tx: &mpsc::Sender<WorkerEvent>,
) -> mpsc::Sender<ProviderStatusTaskResult> {
    let (result_tx, result_rx) = mpsc::channel();
    let event_tx = event_tx.clone();
    runtime.spawn_blocking(move || {
        if let Ok(result) = result_rx.recv() {
            let _ = event_tx.send(WorkerEvent::ProviderStatusResolved(result));
        }
    });
    result_tx
}
