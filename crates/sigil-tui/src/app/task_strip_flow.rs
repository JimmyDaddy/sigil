use super::AppState;

impl AppState {
    pub(crate) fn task_strip_expanded(&self, task_id: &str) -> bool {
        self.review.expanded_task_strip_task_id.as_deref() == Some(task_id)
    }

    pub(crate) fn toggle_task_strip_expansion(&mut self) -> bool {
        let Some(view) = self.task_strip_view() else {
            return false;
        };
        let expanded = self.task_strip_expanded(&view.task_id);
        if !expanded && view.rows.len() <= super::task_sidebar::TASK_STRIP_COLLAPSED_ROW_LIMIT {
            return false;
        }

        if expanded {
            self.review.expanded_task_strip_task_id = None;
            self.last_notice = Some("task list collapsed".to_owned());
        } else {
            let item_count = view.rows.len();
            self.review.expanded_task_strip_task_id = Some(view.task_id);
            self.last_notice = Some(format!("task list expanded · {item_count} items"));
        }
        true
    }
}
