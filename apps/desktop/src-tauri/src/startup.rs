use std::{
    any::Any,
    error::Error,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const STARTUP_LOG_NAME: &str = "startup-error.log";
const STARTUP_MESSAGE_MAX_BYTES: usize = 4096;

/// Removes a stale startup diagnostic before the native event loop is built.
pub fn clear_startup_failure() {
    let Some(path) = startup_failure_path() else {
        return;
    };
    let _ = fs::remove_file(path);
}

/// Records one local, bounded startup error chain when the native shell cannot open.
pub fn record_startup_failure(error: &(dyn Error + 'static)) {
    record_startup_message(&format_error_chain(error));
}

/// Records a bounded panic message when native startup fails before Tauri can render an error.
pub fn record_startup_panic(payload: &(dyn Any + Send)) {
    record_startup_message(&format!("panic: {}", panic_message(payload)));
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("desktop runtime panicked without a string payload")
}

fn record_startup_message(message: &str) {
    let Some(path) = startup_failure_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }

    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let Ok(mut file) = options.open(path) else {
        return;
    };
    let _ = writeln!(file, "{}", bounded_startup_message(message));
}

fn bounded_startup_message(message: &str) -> String {
    let mut output = String::with_capacity(message.len().min(STARTUP_MESSAGE_MAX_BYTES));
    let mut truncated = false;
    for character in message.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if output.len() + character.len_utf8() > STARTUP_MESSAGE_MAX_BYTES - 3 {
            truncated = true;
            break;
        }
        output.push(character);
    }
    if truncated {
        output.push_str("...");
    }
    output
}

#[cfg(target_os = "macos")]
fn startup_failure_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Logs/Sigil").join(STARTUP_LOG_NAME))
}

#[cfg(target_os = "windows")]
fn startup_failure_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|root| root.join("Sigil/logs").join(STARTUP_LOG_NAME))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn startup_failure_path() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(root).join("sigil").join(STARTUP_LOG_NAME));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/state/sigil").join(STARTUP_LOG_NAME))
}

fn format_error_chain(error: &(dyn Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    let mut depth = 0;
    while let Some(error) = source {
        if depth == 8 {
            message.push_str(": further causes omitted");
            break;
        }
        message.push_str(": caused by: ");
        message.push_str(&error.to_string());
        source = error.source();
        depth += 1;
    }
    message
}

#[cfg(test)]
mod tests {
    use super::{
        STARTUP_MESSAGE_MAX_BYTES, bounded_startup_message, format_error_chain, panic_message,
    };

    #[derive(Debug, thiserror::Error)]
    #[error("outer failure")]
    struct OuterFailure {
        #[source]
        source: InnerFailure,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("inner failure")]
    struct InnerFailure;

    #[test]
    fn startup_error_chain_preserves_the_actionable_cause() {
        let error = OuterFailure {
            source: InnerFailure,
        };
        assert_eq!(
            format_error_chain(&error),
            "outer failure: caused by: inner failure"
        );
    }

    #[test]
    fn non_string_panic_payload_is_accepted_without_repanicking() {
        assert_eq!(
            panic_message(&42_u32),
            "desktop runtime panicked without a string payload"
        );
    }

    #[test]
    fn startup_message_is_single_line_utf8_and_byte_bounded() {
        let message = format!("{}\nsecret\0", "界".repeat(2_000));
        let bounded = bounded_startup_message(&message);
        assert!(bounded.len() <= STARTUP_MESSAGE_MAX_BYTES);
        assert!(bounded.ends_with("..."));
        assert!(!bounded.contains('\n'));
        assert!(!bounded.contains('\0'));
    }
}
