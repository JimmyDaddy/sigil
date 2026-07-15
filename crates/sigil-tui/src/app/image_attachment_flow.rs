use std::path::Path;

use anyhow::Result;
use sigil_kernel::{
    ImageAttachment, MAX_IMAGE_ATTACHMENTS_PER_TURN, ModelMessage,
    validate_message_image_attachments,
};
use sigil_runtime::{ControlledImageAttachmentCache, image_path_from_pasted_text};
use uuid::Uuid;

use super::{AppState, ComposerMode, PaneFocus, TimelineRole};

const IMAGE_REJECTED_NOTICE: &str =
    "image rejected; use a valid PNG, JPEG, or WebP within the attachment limits";

impl AppState {
    pub(crate) fn can_accept_image_attachment_input(&self) -> bool {
        self.active_pane == PaneFocus::Composer
            && self.composer.mode == ComposerMode::Build
            && !self.runtime.is_busy
            && !self.has_modal()
            && !self.is_setup_mode()
            && !self.is_config_mode()
            && self.approval.pending.is_none()
            && self.composer.queue_edit_target.is_none()
    }

    pub(crate) fn try_attach_pasted_image_path(&mut self, text: &str) -> bool {
        if !self.can_accept_image_attachment_input() {
            return false;
        }
        let Some(path) = image_path_from_pasted_text(text) else {
            return false;
        };
        if self.attach_image_path(&path).is_err() {
            self.show_image_attachment_notice(IMAGE_REJECTED_NOTICE);
        }
        true
    }

    #[cfg(not(test))]
    pub(crate) fn attach_clipboard_image(&mut self, encoded_png: Vec<u8>) -> Result<()> {
        if !self.can_accept_image_attachment_input() {
            anyhow::bail!("image attachments are only available in an idle Build composer");
        }
        self.admit_image_attachment(
            ControlledImageAttachmentCache::new(self.sigil_paths.attachments_root.clone())
                .ingest_encoded_bytes(Uuid::new_v4().to_string(), encoded_png)?,
        )
    }

    #[cfg(not(test))]
    pub(crate) fn handle_clipboard_image(&mut self, encoded_png: Vec<u8>) {
        if self.attach_clipboard_image(encoded_png).is_err() {
            self.show_image_attachment_notice(IMAGE_REJECTED_NOTICE);
        }
    }

    #[cfg(not(test))]
    pub(crate) fn report_clipboard_image_failure(&mut self) {
        self.show_image_attachment_notice("clipboard image could not be read");
    }

    fn attach_image_path(&mut self, path: &Path) -> Result<()> {
        self.admit_image_attachment(
            ControlledImageAttachmentCache::new(self.sigil_paths.attachments_root.clone())
                .ingest_path(Uuid::new_v4().to_string(), path)?,
        )
    }

    fn admit_image_attachment(&mut self, attachment: ImageAttachment) -> Result<()> {
        if self.composer.image_attachments.len() >= MAX_IMAGE_ATTACHMENTS_PER_TURN {
            anyhow::bail!("image attachment count limit reached");
        }
        let mut message = ModelMessage::user("");
        message
            .image_attachments
            .extend(self.composer.image_attachments.iter().cloned());
        message.image_attachments.push(attachment.clone());
        validate_message_image_attachments(&message)?;

        self.composer.image_attachments.push(attachment);
        self.composer.selected_image_attachment =
            Some(self.composer.image_attachments.len().saturating_sub(1));
        self.show_image_attachment_notice(format!(
            "attached image {} of {}",
            self.composer.image_attachments.len(),
            MAX_IMAGE_ATTACHMENTS_PER_TURN
        ));
        Ok(())
    }

    pub(super) fn select_last_image_attachment(&mut self) -> bool {
        let Some(index) = self.composer.image_attachments.len().checked_sub(1) else {
            return false;
        };
        self.composer.selected_image_attachment = Some(index);
        true
    }

    pub(super) fn move_selected_image_attachment(&mut self, forward: bool) -> bool {
        let len = self.composer.image_attachments.len();
        let Some(selected) = self.composer.selected_image_attachment else {
            return false;
        };
        if len == 0 {
            self.composer.selected_image_attachment = None;
            return false;
        }
        self.composer.selected_image_attachment = Some(if forward {
            (selected + 1) % len
        } else {
            selected.checked_sub(1).unwrap_or(len - 1)
        });
        true
    }

    pub(super) fn clear_selected_image_attachment(&mut self) -> bool {
        self.composer.selected_image_attachment.take().is_some()
    }

    pub(super) fn remove_selected_image_attachment(&mut self) -> bool {
        let Some(selected) = self.composer.selected_image_attachment else {
            return false;
        };
        if selected >= self.composer.image_attachments.len() {
            self.composer.selected_image_attachment = None;
            return false;
        }
        self.composer.image_attachments.remove(selected);
        self.composer.selected_image_attachment = if self.composer.image_attachments.is_empty() {
            None
        } else {
            Some(selected.min(self.composer.image_attachments.len() - 1))
        };
        self.show_image_attachment_notice("image attachment removed");
        true
    }

    pub(super) fn reject_non_build_attachment_submission(&mut self) -> bool {
        if self.composer.image_attachments.is_empty() {
            return false;
        }
        self.show_image_attachment_notice(
            "images can only be sent directly from an idle Build composer; the draft was kept",
        );
        true
    }

    fn show_image_attachment_notice(&mut self, notice: impl Into<String>) {
        let notice = notice.into();
        self.last_notice = Some(notice.clone());
        self.push_timeline(TimelineRole::Notice, notice);
    }
}
