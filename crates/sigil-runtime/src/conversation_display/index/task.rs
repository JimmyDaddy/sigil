use super::*;

#[derive(Debug, Default)]
pub(super) struct TaskMetadataProjection {
    projection: ConversationTaskControlProjection,
    titles: BTreeMap<(String, String), ConversationDisplayRecordPosition>,
    checklists: BTreeMap<String, ConversationDisplayRecordPosition>,
    versions: Vec<(u64, Option<TaskSnapshot>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TaskSnapshot {
    task: ConversationTaskControlV1,
    titles: BTreeMap<String, ConversationDisplayRecordPosition>,
    checklist: Option<ConversationDisplayRecordPosition>,
}

impl TaskMetadataProjection {
    pub(super) fn apply(
        &mut self,
        entry: &SessionLogEntry,
        position: &ConversationDisplayRecordPosition,
    ) -> Result<Option<sigil_kernel::PublicUserInputRequestV1>> {
        self.projection.apply_entry(entry)?;
        if let SessionLogEntry::Control(ControlEntry::TaskPlan(plan)) = entry
            && let Some(task) = self.projection.tasks.get(plan.task_id.as_str())
            && task.plan_version == Some(plan.plan_version)
            && !matches!(task.status.as_str(), "completed" | "cancelled")
        {
            for step in &plan.steps {
                self.titles.insert(
                    (task.task_id.clone(), step.step_id.as_str().to_owned()),
                    position.clone(),
                );
            }
        }
        if let SessionLogEntry::Control(ControlEntry::TaskChecklistUpdatedV1(checklist)) = entry
            && let Some(task) = self.projection.tasks.get(checklist.task_id.as_str())
            && !matches!(task.status.as_str(), "completed" | "cancelled")
        {
            self.checklists
                .insert(task.task_id.clone(), position.clone());
        }
        for task in self.projection.tasks.values_mut() {
            for step in &mut task.steps {
                step.title.clear();
            }
            for item in &mut task.checklist {
                item.text.clear();
            }
        }
        let snapshot = self.projection.current().map(|task| {
            let titles = task
                .steps
                .iter()
                .filter_map(|step| {
                    self.titles
                        .get(&(task.task_id.clone(), step.step_id.clone()))
                        .cloned()
                        .map(|position| (step.step_id.clone(), position))
                })
                .collect();
            let checklist = self.checklists.get(&task.task_id).cloned();
            TaskSnapshot {
                task,
                titles,
                checklist,
            }
        });
        push_version(&mut self.versions, position.sequence, snapshot);
        Ok(self.projection.latest_user_input.take())
    }

    pub(super) fn at(&self, sequence: u64) -> Option<TaskSnapshot> {
        version_at(&self.versions, sequence).cloned().flatten()
    }
}

impl TaskSnapshot {
    pub(super) fn positions(&self) -> impl Iterator<Item = &ConversationDisplayRecordPosition> {
        self.titles.values().chain(self.checklist.iter())
    }

    pub(super) fn hydrate(
        mut self,
        records: &BodyRecords<'_>,
    ) -> Result<ConversationTaskControlV1> {
        for step in &mut self.task.steps {
            let Some(position) = self.titles.get(&step.step_id) else {
                step.title = step.step_id.clone();
                continue;
            };
            let surface = records.surface(position)?;
            let SurfaceBody::TaskTitles { task_id, titles } = surface.as_ref() else {
                bail!("conversation Task title lost its exact plan source");
            };
            if task_id != &self.task.task_id {
                bail!("conversation Task title source changed identity");
            }
            step.title = titles
                .get(&step.step_id)
                .context("conversation Task title source lost its step")?
                .clone();
        }
        if !self.task.checklist.is_empty() {
            let position = self
                .checklist
                .as_ref()
                .context("conversation Task checklist lost its source")?;
            let surface = records.surface(position)?;
            let SurfaceBody::TaskChecklist { task_id, items } = surface.as_ref() else {
                bail!("conversation Task checklist source changed type");
            };
            if task_id != &self.task.task_id {
                bail!("conversation Task checklist source changed identity");
            }
            for item in &mut self.task.checklist {
                item.text = items
                    .iter()
                    .find(|candidate| candidate.item_id == item.item_id)
                    .context("conversation Task checklist source lost its item")?
                    .text
                    .clone();
            }
        }
        Ok(self.task)
    }
}
