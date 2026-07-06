use super::super::{AppState, ModelPickerRefresh};
use crate::runner::WorkerCommand;

impl AppState {
    pub(in crate::app) fn next_background_request_id(&mut self) -> u64 {
        let request_id = self.runtime.next_background_request_id;
        self.runtime.next_background_request_id =
            self.runtime.next_background_request_id.saturating_add(1);
        request_id
    }

    pub(in crate::app) fn cancel_model_picker_refresh(&mut self) {
        if let Some(refresh) = self.runtime.active_model_picker_refresh.take() {
            self.enqueue_worker_command(WorkerCommand::CancelProviderModelsRefresh {
                request_id: refresh.request_id,
            });
        }
    }

    pub(in crate::app) fn apply_provider_models_refresh(
        &mut self,
        request_id: u64,
        base_url: String,
        result: Result<Vec<String>, String>,
    ) -> bool {
        let Some(active) = self.runtime.active_model_picker_refresh.as_ref() else {
            return false;
        };
        if active.request_id != request_id {
            return false;
        }
        let active = self
            .runtime
            .active_model_picker_refresh
            .take()
            .expect("active refresh checked above");
        self.apply_model_picker_refresh(ModelPickerRefresh {
            target: active.target,
            current: active.current,
            base_url,
            result,
        })
    }
}
