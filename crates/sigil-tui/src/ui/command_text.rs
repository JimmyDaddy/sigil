use crate::commands::metadata_slash_commands;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SlashCommandToken<'a> {
    pub(super) token: &'a str,
    pub(super) start: usize,
    pub(super) end: usize,
}

pub(super) fn known_slash_command_token(row: &str) -> Option<SlashCommandToken<'_>> {
    let start = row.len().saturating_sub(row.trim_start().len());
    let token = row[start..].split_whitespace().next()?;
    if !token.starts_with('/') {
        return None;
    }
    metadata_slash_commands()
        .any(|command| command == token)
        .then_some(SlashCommandToken {
            token,
            start,
            end: start + token.len(),
        })
}
