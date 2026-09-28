//! Native-only draft attachment capabilities. No source paths or names cross IPC.
use crate::{commands::DesktopCommandError, state::DesktopAppState};
use base64::Engine as _;
use serde::Serialize;
use sigil_desktop::{
    DesktopImageAttachment, MAX_DESKTOP_IMAGE_BYTES, MAX_DESKTOP_IMAGE_BYTES_PER_TURN,
    MAX_DESKTOP_IMAGES_PER_TURN,
};
use std::{collections::BTreeMap, path::Path};
use tauri::State;
use tauri_plugin_dialog::DialogExt;

const MAX_DRAFT_IMAGES: usize = 64;
const MAX_DRAFT_BYTES: u64 = MAX_DESKTOP_IMAGE_BYTES_PER_TURN * 16;

#[derive(Default)]
pub(crate) struct DraftImageRegistry {
    images: BTreeMap<(String, String), DesktopImageAttachment>,
}

impl DraftImageRegistry {
    fn insert(
        &mut self,
        workspace: &str,
        image: DesktopImageAttachment,
    ) -> Result<(), DesktopCommandError> {
        if self.images.len() >= MAX_DRAFT_IMAGES
            || self
                .images
                .values()
                .map(|image| image.byte_len)
                .sum::<u64>()
                + image.byte_len
                > MAX_DRAFT_BYTES
        {
            return Err(image_error(
                "image_drafts_full",
                "Remove an unused draft image before attaching another.",
            ));
        }
        self.images
            .insert((workspace.to_owned(), image.attachment_id.clone()), image);
        Ok(())
    }

    pub(crate) fn resolve(
        &self,
        workspace: &str,
        handles: &[String],
    ) -> Result<Vec<DesktopImageAttachment>, DesktopCommandError> {
        if handles.len() > MAX_DESKTOP_IMAGES_PER_TURN {
            return Err(image_error(
                "image_limit",
                "A message accepts up to four images.",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        let images = handles
            .iter()
            .map(|handle| {
                if !seen.insert(handle) {
                    return Err(image_error(
                        "image_reference_invalid",
                        "An image was selected more than once.",
                    ));
                }
                self.images
                    .get(&(workspace.to_owned(), handle.clone()))
                    .cloned()
                    .ok_or_else(|| {
                        image_error(
                            "image_reference_unavailable",
                            "Attach the original image again.",
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if images.iter().map(|image| image.byte_len).sum::<u64>() > MAX_DESKTOP_IMAGE_BYTES_PER_TURN
        {
            return Err(image_error(
                "image_limit",
                "Images in one message must total no more than 24 MiB.",
            ));
        }
        Ok(images)
    }

    fn release(&mut self, workspace: &str, handles: &[String]) {
        for handle in handles {
            self.images.remove(&(workspace.to_owned(), handle.clone()));
        }
    }

    pub(crate) fn clear_workspace(&mut self, workspace: &str) {
        self.images.retain(|(id, _), _| id != workspace);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImageReference {
    pub(crate) attachment_id: String,
    pub(crate) mime_type: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) byte_len: u64,
}

impl From<DesktopImageAttachment> for ImageReference {
    fn from(image: DesktopImageAttachment) -> Self {
        Self {
            attachment_id: image.attachment_id,
            mime_type: format!("image/{}", image.mime_type),
            width: image.width,
            height: image.height,
            byte_len: image.byte_len,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DraftImage {
    #[serde(flatten)]
    reference: ImageReference,
    preview_data_url: String,
}

fn image_error(code: &'static str, message: &'static str) -> DesktopCommandError {
    DesktopCommandError::new(code, message)
}

async fn ingest(
    workspace_id: &str,
    bytes: Vec<u8>,
    state: &DesktopAppState,
) -> Result<DraftImage, DesktopCommandError> {
    if bytes.is_empty() || bytes.len() > MAX_DESKTOP_IMAGE_BYTES {
        return Err(image_error(
            "image_limit",
            "Choose a PNG, JPEG, or WebP image no larger than 8 MiB.",
        ));
    }
    let client = state.manager.client(workspace_id).map_err(|_| {
        image_error(
            "workspace_unavailable",
            "Reopen the workspace to attach an image.",
        )
    })?;
    let image = client.ingest_image(bytes.clone()).await.map_err(|_| image_error("image_invalid", "This image could not be attached. Use a valid PNG, JPEG, or WebP within the image limits."))?;
    let reference = ImageReference::from(image.clone());
    if !matches!(
        reference.mime_type.as_str(),
        "image/png" | "image/jpeg" | "image/webp"
    ) {
        return Err(image_error(
            "image_invalid",
            "The image format is unsupported.",
        ));
    }
    let preview_data_url = format!(
        "data:{};base64,{}",
        reference.mime_type,
        base64::engine::general_purpose::STANDARD.encode(bytes)
    );
    let mut images = state
        .images
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Serialize the post-upload admission with workspace-close cleanup.
    state
        .manager
        .client(workspace_id)
        .map_err(|_| image_error("workspace_unavailable", "The workspace was closed."))?;
    images.insert(workspace_id, image)?;
    Ok(DraftImage {
        reference,
        preview_data_url,
    })
}

#[tauri::command]
pub(crate) async fn desktop_ingest_image(
    workspace_id: String,
    bytes: Vec<u8>,
    state: State<'_, DesktopAppState>,
) -> Result<DraftImage, DesktopCommandError> {
    ingest(&workspace_id, bytes, &state).await
}

#[tauri::command]
pub(crate) async fn desktop_pick_image(
    app: tauri::AppHandle,
    workspace_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<Option<DraftImage>, DesktopCommandError> {
    state.manager.client(&workspace_id).map_err(|_| {
        image_error(
            "workspace_unavailable",
            "Open a workspace before attaching an image.",
        )
    })?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
        .pick_file(move |file| {
            let _ = sender.send(file);
        });
    let Some(file) = receiver.await.map_err(|_| {
        image_error(
            "image_picker_unavailable",
            "The image picker could not be opened.",
        )
    })?
    else {
        return Ok(None);
    };
    let path = file
        .into_path()
        .map_err(|_| image_error("image_invalid", "Select a local image file."))?;
    let bytes = tokio::task::spawn_blocking(move || read_selected_image(&path))
        .await
        .map_err(|_| image_error("image_read_failed", "The image could not be read."))??;
    ingest(&workspace_id, bytes, &state).await.map(Some)
}

fn read_selected_image(path: &Path) -> Result<Vec<u8>, DesktopCommandError> {
    crate::selected_file::read_selected_file(path, MAX_DESKTOP_IMAGE_BYTES as u64).map_err(|_| {
        image_error(
            "image_read_failed",
            "Select a regular local image no larger than 8 MiB.",
        )
    })
}

#[tauri::command]
pub(crate) fn desktop_release_images(
    workspace_id: String,
    handles: Vec<String>,
    state: State<'_, DesktopAppState>,
) {
    let mut images = state
        .images
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    images.release(&workspace_id, &handles);
}

#[tauri::command]
pub(crate) async fn desktop_message_image(
    workspace_id: String,
    session_id: String,
    display_id: String,
    attachment_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<String, DesktopCommandError> {
    let client = state.manager.client(&workspace_id).map_err(|_| {
        image_error(
            "workspace_unavailable",
            "Reopen the workspace to view this image.",
        )
    })?;
    let content = client
        .message_image(&session_id, &display_id, &attachment_id)
        .await
        .map_err(|_| {
            image_error(
                "image_unavailable",
                "The saved image is missing or changed. Attach the original again.",
            )
        })?;
    Ok(format!(
        "data:{};base64,{}",
        content.mime_type, content.data_base64
    ))
}

#[cfg(test)]
#[path = "tests/image_attachments_tests.rs"]
mod tests;
