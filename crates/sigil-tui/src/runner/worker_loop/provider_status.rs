use super::*;

pub(in crate::runner) fn drain_provider_status_results(
    provider_status_rx: &mpsc::Receiver<ProviderStatusTaskResult>,
    provider_status_tasks: &mut ProviderStatusTaskManager,
    message_tx: &mpsc::Sender<WorkerMessage>,
) {
    while let Ok(status_result) = provider_status_rx.try_recv() {
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
        }
    }
}
