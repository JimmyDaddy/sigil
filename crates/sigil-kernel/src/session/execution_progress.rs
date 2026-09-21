use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};

use super::ControlEntry;
use crate::TaskId;

const MAX_RUN_BINDINGS: usize = super::TOOL_OUTPUT_PRESSURE_HARD_MAX_RESULTS;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExecutionScope {
    Direct {
        task_id: TaskId,
        admission_id: String,
    },
}

impl ExecutionScope {
    fn task_id(&self) -> &TaskId {
        match self {
            Self::Direct { task_id, .. } => task_id,
        }
    }
}

/// Body-free execution identities derived inside the existing active Session owner.
#[derive(Debug, Clone, Default)]
pub(super) struct ExecutionProgress {
    scopes: BTreeMap<String, ExecutionScope>,
    continuation_roots: BTreeMap<String, String>,
}

impl ExecutionProgress {
    fn bind(&mut self, run_id: String, scope: ExecutionScope) -> Result<()> {
        if let Some(existing) = self.scopes.get(&run_id) {
            if existing != &scope {
                bail!("execution progress run has conflicting durable scope");
            }
            return Ok(());
        }
        if self.scopes.len() == MAX_RUN_BINDINGS {
            bail!("execution progress exceeded its durable run binding limit");
        }
        self.scopes.insert(run_id, scope);
        Ok(())
    }

    pub(super) fn apply_control(&mut self, control: &ControlEntry) -> Result<()> {
        match control {
            ControlEntry::TaskDirectExecutionAttemptV1(attempt) => {
                attempt.validate()?;
                self.bind(
                    crate::task_direct_execution_logical_run_id(&attempt.attempt_id),
                    ExecutionScope::Direct {
                        task_id: attempt.task_id.clone(),
                        admission_id: attempt.admission_id.clone(),
                    },
                )?;
            }
            ControlEntry::UserInputContinuationStarted(entry) => {
                entry.validate()?;
                let run = entry.continuation_logical_run_id.as_str().to_owned();
                let root = entry.identity.root_logical_run_id.as_str().to_owned();
                if let Some(existing) = self.continuation_roots.get(&run) {
                    if existing != &root {
                        bail!("execution progress continuation has conflicting root identity");
                    }
                } else {
                    if self.continuation_roots.len() == MAX_RUN_BINDINGS {
                        bail!("execution progress exceeded its continuation binding limit");
                    }
                    self.continuation_roots.insert(run, root);
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn recorded_evidence_run_ids(&self, run_id: &str) -> BTreeSet<String> {
        let root = self
            .continuation_roots
            .get(run_id)
            .map_or(run_id, String::as_str);
        let mut runs = BTreeSet::from([run_id.to_owned(), root.to_owned()]);
        if let Some(scope) = self.scopes.get(root) {
            runs.extend(
                self.scopes
                    .iter()
                    .filter(|(_, candidate)| candidate.task_id() == scope.task_id())
                    .map(|(run, _)| run.clone()),
            );
        }
        runs.extend(
            self.continuation_roots
                .iter()
                .filter(|(_, source)| runs.contains(*source))
                .map(|(run, _)| run.clone())
                .collect::<Vec<_>>(),
        );
        runs
    }
}

#[cfg(test)]
#[path = "execution_progress/tests/scope_tests.rs"]
mod scope_tests;
