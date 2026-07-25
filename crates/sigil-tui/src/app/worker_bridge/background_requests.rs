use super::super::{AppState, ModelPickerRefresh, modal_flow::ModelPickerTarget};
use crate::runner::WorkerCommand;
use sigil_kernel::RootConfig;
#[cfg(not(test))]
use sigil_runtime::provider_connections::{
    ConfiguredProviderCredentialStore, connection_inventory_with_cancellation,
};
use sigil_runtime::provider_connections::{
    ModelCatalogResult, ProcessCredentialEnvironment, connection_inventory_offline,
};
use std::sync::mpsc::TryRecvError;

impl AppState {
    pub(in crate::app) fn schedule_connection_inventory_refresh(
        &mut self,
        root_config: &RootConfig,
    ) {
        self.runtime.connection_inventory_revision =
            self.runtime.connection_inventory_revision.saturating_add(1);
        #[cfg(not(test))]
        let revision = self.runtime.connection_inventory_revision;
        self.runtime.connection_inventory = Some(connection_inventory_offline(
            root_config,
            &ProcessCredentialEnvironment,
        ));
        #[cfg(test)]
        {
            self.runtime.connection_inventory_rx = None;
        }

        #[cfg(not(test))]
        {
            if let Some(worker) = self.runtime.connection_inventory_worker.as_mut() {
                if worker
                    .handle
                    .as_ref()
                    .is_some_and(std::thread::JoinHandle::is_finished)
                {
                    self.runtime
                        .connection_inventory_worker
                        .take()
                        .expect("finished inventory worker was present")
                        .finish();
                    self.runtime.connection_inventory_rx = None;
                } else {
                    worker
                        .cancelled
                        .store(true, std::sync::atomic::Ordering::Release);
                    self.runtime.pending_connection_inventory_config = Some(root_config.clone());
                    return;
                }
            }
            self.start_connection_inventory_worker(root_config.clone(), revision);
        }
    }

    #[cfg(not(test))]
    fn start_connection_inventory_worker(&mut self, root_config: RootConfig, revision: u64) {
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancelled = std::sync::Arc::clone(&cancelled);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::Builder::new()
            .name("sigil-connection-inventory".to_owned())
            .spawn(move || {
                let Some(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok()
                else {
                    return;
                };
                let inventory = runtime.block_on(connection_inventory_with_cancellation(
                    &root_config,
                    &ConfiguredProviderCredentialStore::from_root_config(&root_config),
                    &ProcessCredentialEnvironment,
                    &worker_cancelled,
                ));
                runtime.shutdown_timeout(std::time::Duration::from_millis(100));
                if !worker_cancelled.load(std::sync::atomic::Ordering::Acquire) {
                    let _ = sender.send((revision, inventory));
                }
            });
        if let Ok(handle) = handle {
            self.runtime.connection_inventory_rx = Some(receiver);
            self.runtime.connection_inventory_worker =
                Some(super::super::state::ConnectionInventoryWorker {
                    cancelled,
                    handle: Some(handle),
                });
        }
    }

    #[cfg(not(test))]
    fn finish_connection_inventory_worker(&mut self) -> bool {
        if let Some(worker) = self.runtime.connection_inventory_worker.take() {
            worker.finish();
        }
        self.runtime.connection_inventory_rx = None;
        let Some(root_config) = self.runtime.pending_connection_inventory_config.take() else {
            return false;
        };
        self.schedule_connection_inventory_refresh(&root_config);
        true
    }

    pub(in crate::app) fn poll_connection_inventory(&mut self) -> bool {
        let received = match self.runtime.connection_inventory_rx.as_ref() {
            Some(receiver) => receiver.try_recv(),
            None => return false,
        };
        match received {
            Ok((revision, inventory)) => {
                let is_current = revision == self.runtime.connection_inventory_revision;
                if is_current {
                    self.runtime.connection_inventory = Some(inventory);
                }
                #[cfg(test)]
                {
                    self.runtime.connection_inventory_rx = None;
                }
                #[cfg(not(test))]
                let restarted = self.finish_connection_inventory_worker();
                #[cfg(test)]
                let restarted = false;
                is_current || restarted
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                #[cfg(test)]
                {
                    self.runtime.connection_inventory_rx = None;
                    false
                }
                #[cfg(not(test))]
                {
                    self.finish_connection_inventory_worker()
                }
            }
        }
    }

    pub(in crate::app) fn poll_setup_model_catalog(&mut self) -> bool {
        let received = match self.runtime.setup_model_catalog_rx.as_ref() {
            Some(receiver) => receiver.try_recv(),
            None => return false,
        };
        match received {
            Ok(Ok(result)) => {
                self.runtime.setup_model_catalog_rx = None;
                self.apply_connection_models_refresh(result)
            }
            Ok(Err(message)) => {
                self.runtime.setup_model_catalog_rx = None;
                self.apply_setup_model_catalog_failure(message)
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.runtime.setup_model_catalog_rx = None;
                self.apply_setup_model_catalog_failure(
                    "model catalog worker disconnected".to_owned(),
                )
            }
        }
    }

    fn apply_setup_model_catalog_failure(&mut self, message: String) -> bool {
        let Some(active) = self.runtime.active_model_picker_refresh.take() else {
            return false;
        };
        if active.target != ModelPickerTarget::Setup {
            self.runtime.active_model_picker_refresh = Some(active);
            return false;
        }
        if let Some(super::super::modal_flow::ModalState::ModelPicker(picker)) =
            self.modal_state.as_mut()
        {
            picker.catalog_state =
                super::super::modal_flow::ModelCatalogState::Error(message.clone());
            picker.manual_entry_allowed = false;
        }
        if let Some(setup) = self.setup_state.as_mut() {
            setup.catalog_admission = None;
        }
        self.last_notice = Some(message.clone());
        self.push_event("model_list:error", message);
        true
    }

    pub(in crate::app) fn next_background_request_id(&mut self) -> u64 {
        let request_id = self.runtime.next_background_request_id;
        self.runtime.next_background_request_id =
            self.runtime.next_background_request_id.saturating_add(1);
        request_id
    }

    pub(in crate::app) fn cancel_model_picker_refresh(&mut self) {
        if let Some(refresh) = self.runtime.active_model_picker_refresh.take() {
            if refresh.target == ModelPickerTarget::Setup {
                self.runtime.setup_model_catalog_rx = None;
            } else {
                self.enqueue_worker_command(WorkerCommand::CancelProviderModelsRefresh {
                    request_id: refresh.request_id,
                });
            }
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
        if active.request_id != request_id || active.base_url != base_url {
            return false;
        }
        let active = self
            .runtime
            .active_model_picker_refresh
            .take()
            .expect("active refresh checked above");
        self.apply_model_picker_refresh(ModelPickerRefresh {
            target: active.target,
            provider_name: active.provider_name,
            current: active.current,
            base_url,
            result,
        })
    }

    pub(in crate::app) fn apply_connection_models_refresh(
        &mut self,
        result: ModelCatalogResult,
    ) -> bool {
        let Some(active) = self.runtime.active_model_picker_refresh.as_ref() else {
            return false;
        };
        let revision_matches = match active.target {
            ModelPickerTarget::Setup => self
                .setup_state
                .as_ref()
                .is_some_and(|state| state.draft_revision == result.draft_revision),
            ModelPickerTarget::Provider => self.config_state.as_ref().map_or_else(
                || result.draft_revision == 0,
                |state| {
                    state.draft_revision == result.draft_revision
                        && state.draft.selected_connection_id == result.connection_id
                },
            ),
            ModelPickerTarget::ProviderFim => false,
        };
        if active.request_id != result.request_id
            || active.connection_id.as_ref() != Some(&result.connection_id)
            || active.draft_revision != result.draft_revision
            || active.connection_fingerprint.as_deref()
                != Some(result.connection_fingerprint.as_str())
            || !revision_matches
        {
            return false;
        }
        self.runtime.active_model_picker_refresh = None;
        self.apply_connection_model_catalog(result)
    }
}
