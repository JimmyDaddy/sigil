#[cfg(not(test))]
use anyhow::Context;
use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClipboardCopyOutcome {
    Copied,
    Unavailable(String),
}

pub(crate) fn copy_text(text: &str, osc52_enabled: bool) -> ClipboardCopyOutcome {
    copy_text_with(
        text,
        osc52_enabled,
        |text| {
            let result = write_osc52_clipboard(text);
            if let Err(error) = &result {
                tracing::debug!(%error, "failed to write OSC52 clipboard sequence");
            }
            result
        },
        |text| {
            let result = write_system_clipboard(text);
            if let Err(error) = &result {
                tracing::debug!(%error, "failed to write system clipboard");
            }
            result
        },
    )
}

fn copy_text_with<Osc52, System>(
    text: &str,
    osc52_enabled: bool,
    mut write_osc52: Osc52,
    mut write_system: System,
) -> ClipboardCopyOutcome
where
    Osc52: FnMut(&str) -> Result<()>,
    System: FnMut(&str) -> Result<()>,
{
    let osc52_copied = osc52_enabled && write_osc52(text).is_ok();
    let system_copied = write_system(text).is_ok();

    if osc52_copied || system_copied {
        ClipboardCopyOutcome::Copied
    } else if osc52_enabled {
        ClipboardCopyOutcome::Unavailable(
            "system clipboard unavailable; OSC52 write failed".to_owned(),
        )
    } else {
        ClipboardCopyOutcome::Unavailable("system clipboard unavailable; OSC52 disabled".to_owned())
    }
}

#[cfg(not(test))]
fn write_osc52_clipboard(text: &str) -> Result<()> {
    use std::io::Write as _;

    let mut stdout = std::io::stdout();
    let sequence = wrap_osc52_for_multiplexer(
        osc52_clipboard_sequence(text),
        non_empty_env("TMUX"),
        non_empty_env("STY"),
    );
    stdout
        .write_all(sequence.as_bytes())
        .context("failed to write OSC52 clipboard sequence")?;
    stdout
        .flush()
        .context("failed to flush OSC52 clipboard sequence")
}

#[cfg(test)]
fn write_osc52_clipboard(_text: &str) -> Result<()> {
    Ok(())
}

#[cfg(not(test))]
fn write_system_clipboard(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().context("failed to open the system clipboard")?;
    clipboard
        .set_text(text.to_owned())
        .context("failed to write text to the system clipboard")
}

#[cfg(test)]
fn write_system_clipboard(_text: &str) -> Result<()> {
    Ok(())
}

fn osc52_clipboard_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))
}

fn wrap_osc52_for_multiplexer(sequence: String, tmux: bool, screen: bool) -> String {
    if tmux {
        format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"))
    } else if screen {
        format!("\x1bP{sequence}\x1b\\")
    } else {
        sequence
    }
}

#[cfg(not(test))]
fn non_empty_env(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0b0000_0011) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((second & 0b0000_1111) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(third & 0b0011_1111) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

#[cfg(test)]
#[path = "tests/clipboard_tests.rs"]
mod tests;
