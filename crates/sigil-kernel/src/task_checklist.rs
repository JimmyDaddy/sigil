use std::collections::BTreeSet;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{PlanDraftStep, TaskId, safe_persistence_text};

/// Reserved internal tool used by an active Task executor to refresh display-only progress.
pub const UPDATE_TASK_CHECKLIST_TOOL_NAME: &str = "update_task_checklist";
/// An empty model update clears the progress checklist.
pub const TASK_CHECKLIST_MIN_ITEMS: usize = 0;
/// Product cap aligned with the compact task surfaces.
pub const TASK_CHECKLIST_MAX_ITEMS: usize = 32;
/// Maximum user-visible text in one checklist item.
pub const TASK_CHECKLIST_ITEM_MAX_CHARS: usize = 240;

/// Non-authoritative progress state for one display checklist item.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskChecklistItemStatusV1 {
    Pending,
    InProgress,
    Completed,
}

/// One bounded display-only checklist item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskChecklistItemV1 {
    pub item_id: String,
    pub text: String,
    pub status: TaskChecklistItemStatusV1,
}

/// Append-only replacement of a Task's display-only progress checklist.
///
/// This contract never grants execution, permission, scheduling, dependency, role, or completion
/// authority. Invalid or absent checklist data cannot block the owning Task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskChecklistUpdatedV1 {
    pub task_id: TaskId,
    pub revision: u32,
    pub items: Vec<TaskChecklistItemV1>,
}

impl TaskChecklistUpdatedV1 {
    /// Validates bounded display data and monotonic-progress shape.
    pub fn validate(&self) -> Result<()> {
        if self.revision == 0 {
            bail!("task checklist revision must start at one");
        }
        if !(TASK_CHECKLIST_MIN_ITEMS..=TASK_CHECKLIST_MAX_ITEMS).contains(&self.items.len()) {
            bail!(
                "task checklist must contain between {TASK_CHECKLIST_MIN_ITEMS} and {TASK_CHECKLIST_MAX_ITEMS} items"
            );
        }
        let mut ids = BTreeSet::new();
        for item in &self.items {
            if item.item_id.is_empty()
                || item.item_id.len() > 128
                || item.item_id.chars().any(char::is_whitespace)
            {
                bail!("task checklist item id is invalid");
            }
            if !ids.insert(item.item_id.as_str()) {
                bail!("task checklist item ids must be unique");
            }
            let text = item.text.trim();
            if text.is_empty()
                || text.chars().count() > TASK_CHECKLIST_ITEM_MAX_CHARS
                || safe_persistence_text(text) != text
            {
                bail!("task checklist item text is invalid");
            }
        }
        Ok(())
    }
}

/// Host binding for the optional checklist-update tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskChecklistUpdateContextV1 {
    pub task_id: TaskId,
    pub current_revision: u32,
}

/// Builds an initial checklist from typed Plan display metadata.
///
/// Plan steps are used only as labels. No dependency, role, capability, intent, permission, or
/// scheduling data is copied into execution authority. Plans with fewer than two usable labels do
/// not produce a checklist.
#[must_use]
pub fn task_checklist_from_plan_steps(
    task_id: TaskId,
    steps: &[PlanDraftStep],
) -> Option<TaskChecklistUpdatedV1> {
    let items = steps
        .iter()
        .filter_map(|step| {
            let text = step.title.trim();
            (!text.is_empty()).then(|| TaskChecklistItemV1 {
                item_id: crate::stable_event_uuid(
                    "sigil-task-checklist-plan-item-v1",
                    &format!("{}\n{}", task_id.as_str(), step.step_id),
                ),
                text: safe_persistence_text(text)
                    .chars()
                    .take(TASK_CHECKLIST_ITEM_MAX_CHARS)
                    .collect(),
                status: TaskChecklistItemStatusV1::Pending,
            })
        })
        .take(TASK_CHECKLIST_MAX_ITEMS)
        .collect::<Vec<_>>();
    if items.len() < 2 {
        return None;
    }
    let checklist = TaskChecklistUpdatedV1 {
        task_id,
        revision: 1,
        items,
    };
    checklist.validate().ok().map(|()| checklist)
}

/// Returns the provider-neutral schema for the optional checklist progress tool.
#[must_use]
pub fn update_task_checklist_tool_spec() -> crate::ToolSpec {
    crate::ToolSpec {
        name: UPDATE_TASK_CHECKLIST_TOOL_NAME.to_owned(),
        description: "Track task progress with a short ordered checklist. Send the complete list of items with text and status (pending, in_progress, completed); mark each item when its work is done. Send an empty list to clear it. Skip this tool for trivial work. The checklist reports progress and does not authorize tools or prove verification passed.".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["items"],
            "properties": {
                "items": {
                    "type": "array",
                    "minItems": TASK_CHECKLIST_MIN_ITEMS,
                    "maxItems": TASK_CHECKLIST_MAX_ITEMS,
                    "items": {
                        "type": "object",
                        "required": ["text", "status"],
                        "properties": {
                            "text": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": TASK_CHECKLIST_ITEM_MAX_CHARS
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"]
                            }
                        }
                    }
                }
            }
        }),
        category: crate::ToolCategory::Custom,
        access: crate::ToolAccess::Read,
        network_effect: None,
        preview: crate::ToolPreviewCapability::None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawTaskChecklistArgs {
    items: Vec<RawTaskChecklistItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawTaskChecklistItem {
    text: String,
    status: TaskChecklistItemStatusV1,
}

/// Parses one model update into bounded, host-identified display data.
pub fn task_checklist_update_entry(
    context: &TaskChecklistUpdateContextV1,
    call: &crate::ToolCall,
) -> Result<TaskChecklistUpdatedV1> {
    if call.name != UPDATE_TASK_CHECKLIST_TOOL_NAME {
        bail!("expected {UPDATE_TASK_CHECKLIST_TOOL_NAME} tool call");
    }
    let args: RawTaskChecklistArgs = serde_json::from_str(&call.args_json)
        .map_err(|error| anyhow::anyhow!("invalid task checklist arguments: {error}"))?;
    let revision = context
        .current_revision
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("task checklist revision overflow"))?;
    let items = args
        .items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let text = item.text.trim().to_owned();
            TaskChecklistItemV1 {
                item_id: crate::stable_event_uuid(
                    "sigil-task-checklist-model-item-v1",
                    &format!(
                        "{}\n{}\n{}\n{}",
                        context.task_id.as_str(),
                        revision,
                        index,
                        text
                    ),
                ),
                text,
                status: item.status,
            }
        })
        .collect();
    let entry = TaskChecklistUpdatedV1 {
        task_id: context.task_id.clone(),
        revision,
        items,
    };
    entry.validate()?;
    Ok(entry)
}

#[cfg(test)]
#[path = "tests/task_checklist_tests.rs"]
mod tests;
