use std::{fs, io::ErrorKind, path::Path};

use anyhow::{Context, Result};

use super::AppState;

const INPUT_HISTORY_LIMIT: usize = 100;

impl AppState {
    pub(super) fn load_input_history(&mut self) {
        if !input_history_persistence_enabled() {
            return;
        }
        if let Ok(history) = read_input_history(&self.input_history_path(), INPUT_HISTORY_LIMIT) {
            self.composer.input_history = history;
        }
    }

    pub(super) fn record_input_history(&mut self, prompt: String) {
        if !should_record_input_history_entry(&prompt) {
            return;
        }
        if !push_input_history_entry(
            &mut self.composer.input_history,
            prompt,
            INPUT_HISTORY_LIMIT,
        ) {
            return;
        }
        self.persist_input_history();
    }

    pub(super) fn reset_input_history_navigation(&mut self) {
        self.composer.input_history_index = None;
        self.composer.input_history_draft = None;
    }

    pub(super) fn navigate_input_history(&mut self, older: bool) {
        if self.composer.input_history.is_empty() {
            return;
        }

        if older {
            match self.composer.input_history_index {
                Some(0) => {}
                Some(index) => {
                    self.composer.input_history_index = Some(index - 1);
                }
                None => {
                    self.composer.input_history_draft = Some(self.composer.input.clone());
                    self.composer.input_history_index = Some(self.composer.input_history.len() - 1);
                }
            }
        } else {
            match self.composer.input_history_index {
                Some(index) if index + 1 < self.composer.input_history.len() => {
                    self.composer.input_history_index = Some(index + 1);
                }
                Some(_) => {
                    let draft = self.composer.input_history_draft.take().unwrap_or_default();
                    self.set_input_and_cursor(draft);
                    self.composer.input_history_index = None;
                    self.reset_slash_selector();
                    return;
                }
                None => return,
            }
        }

        if let Some(index) = self.composer.input_history_index
            && let Some(value) = self.composer.input_history.get(index)
        {
            self.set_input_and_cursor(value.clone());
            self.discard_cleared_input_draft();
            self.reset_slash_selector();
        }
    }

    fn input_history_path(&self) -> std::path::PathBuf {
        self.sigil_paths.input_history_file.clone()
    }

    fn persist_input_history(&self) {
        if !input_history_persistence_enabled() {
            return;
        }
        let _ = write_input_history(&self.input_history_path(), &self.composer.input_history);
    }
}

fn input_history_persistence_enabled() -> bool {
    #[cfg(test)]
    {
        std::env::var_os("SIGIL_TUI_TEST_PERSIST_INPUT_HISTORY").is_some()
    }
    #[cfg(not(test))]
    {
        true
    }
}

fn push_input_history_entry(history: &mut Vec<String>, prompt: String, limit: usize) -> bool {
    if prompt.is_empty() || history.last().map(|last| last == &prompt).unwrap_or(false) {
        return false;
    }
    history.push(prompt);
    if history.len() > limit {
        let overflow = history.len() - limit;
        history.drain(0..overflow);
    }
    true
}

fn should_record_input_history_entry(prompt: &str) -> bool {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return false;
    }

    let token = trimmed
        .split_once(char::is_whitespace)
        .map(|(token, _)| token)
        .unwrap_or(trimmed);

    !matches!(token, "/quit" | "/q" | "/exit" | "/new" | "/feedback")
}

fn read_input_history(path: &Path, limit: usize) -> Result<Vec<String>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("read input history {}", path.display()));
        }
    };

    let mut history = Vec::new();
    for line in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Ok(prompt) = serde_json::from_str::<String>(line)
            && should_record_input_history_entry(&prompt)
        {
            push_input_history_entry(&mut history, prompt, limit);
        }
    }
    Ok(history)
}

fn write_input_history(path: &Path, history: &[String]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create input history dir {}", parent.display()))?;
    }
    let mut content = String::new();
    for prompt in history {
        let safe_prompt = sigil_kernel::safe_persistence_text(prompt);
        content.push_str(&serde_json::to_string(&safe_prompt)?);
        content.push('\n');
    }

    let temp_path = path.with_extension("jsonl.tmp");
    fs::write(&temp_path, content)
        .with_context(|| format!("write input history temp {}", temp_path.display()))?;
    fs::rename(&temp_path, path)
        .with_context(|| format!("replace input history {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
#[path = "tests/input_history_tests.rs"]
mod tests;
