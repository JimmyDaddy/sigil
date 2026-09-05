//! Shared token-count parsing and display helpers for user-facing configuration flows.

use std::fmt;

/// Common context-window choices shown by the setup and configuration surfaces.
pub(crate) const CONTEXT_WINDOW_PRESETS: [&str; 5] = ["", "64K", "128K", "256K", "1M"];
/// Common output budgets offered by the setup flow.
pub(crate) const MAX_OUTPUT_TOKEN_PRESETS: [&str; 8] =
    ["", "4K", "8K", "16K", "32K", "64K", "128K", "256K"];

/// Parses an optional token count from a human-friendly value such as \`256K\` or \`1M\`.
///
/// \`K\` and \`M\` use decimal units to match the values already persisted by the provider
/// connection configuration (\`256K\` = 256,000 tokens). An empty value means automatic.
pub(crate) fn parse_optional_token_count(value: &str) -> Result<Option<u32>, TokenCountError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }

    let (digits, multiplier) = match value.chars().last() {
        Some(suffix @ ('k' | 'K')) => (&value[..value.len() - suffix.len_utf8()], 1_000_u64),
        Some(suffix @ ('m' | 'M')) => (&value[..value.len() - suffix.len_utf8()], 1_000_000_u64),
        Some(_) => (value, 1_u64),
        None => unreachable!("empty token count handled above"),
    };
    if digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit()) {
        return Err(TokenCountError::Invalid(value.to_owned()));
    }

    let base = digits
        .parse::<u64>()
        .map_err(|_| TokenCountError::Invalid(value.to_owned()))?;
    let tokens = base
        .checked_mul(multiplier)
        .filter(|tokens| *tokens <= u32::MAX as u64)
        .ok_or_else(|| TokenCountError::OutOfRange(value.to_owned()))?;
    if tokens == 0 {
        return Err(TokenCountError::Zero);
    }
    Ok(Some(tokens as u32))
}

pub(crate) fn is_token_count_character(character: char) -> bool {
    character.is_ascii_digit() || matches!(character, 'k' | 'K' | 'm' | 'M')
}

/// Returns a stable display form for a token count while preserving non-preset custom values.
pub(crate) fn token_count_display(value: &str, automatic: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return automatic.to_owned();
    }
    match parse_optional_token_count(trimmed) {
        Ok(Some(1_000_000)) => "1M".to_owned(),
        Ok(Some(256_000)) => "256K".to_owned(),
        Ok(Some(128_000)) => "128K".to_owned(),
        Ok(Some(64_000)) => "64K".to_owned(),
        Ok(Some(32_000)) => "32K".to_owned(),
        Ok(Some(16_000)) => "16K".to_owned(),
        Ok(Some(8_000)) => "8K".to_owned(),
        Ok(Some(4_000)) => "4K".to_owned(),
        Ok(Some(tokens)) => format!("{tokens} tokens"),
        Ok(None) => automatic.to_owned(),
        Err(_) => trimmed.to_owned(),
    }
}

/// Returns the max-output form used in configuration rows, including a unit suffix for
/// human-friendly preset values.
pub(crate) fn max_output_token_display(value: &str, automatic: &str) -> String {
    let display = token_count_display(value, automatic);
    if display == automatic || display.ends_with(" tokens") {
        display
    } else {
        format!("{display} tokens")
    }
}

/// Cycles through the common context-window presets. Unknown custom values return automatic.
pub(crate) fn cycle_context_window_preset(value: &str, backwards: bool) -> &'static str {
    cycle_token_preset(value, &CONTEXT_WINDOW_PRESETS, backwards)
}

pub(crate) fn cycle_token_preset(
    value: &str,
    presets: &[&'static str],
    backwards: bool,
) -> &'static str {
    let current = parse_optional_token_count(value).ok().flatten();
    let Some(index) = presets
        .iter()
        .position(|preset| parse_optional_token_count(preset).ok().flatten() == current)
    else {
        return presets[0];
    };
    let len = presets.len();
    let next = if backwards {
        (index + len - 1) % len
    } else {
        (index + 1) % len
    };
    presets[next]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TokenCountError {
    Invalid(String),
    OutOfRange(String),
    Zero,
}

impl fmt::Display for TokenCountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(value) => write!(
                formatter,
                "invalid token count {value:?}; use a positive integer with optional K/M suffix"
            ),
            Self::OutOfRange(value) => {
                write!(
                    formatter,
                    "token count {value:?} is outside the supported range"
                )
            }
            Self::Zero => formatter.write_str("token count must be greater than 0"),
        }
    }
}

#[cfg(test)]
#[path = "tests/token_units_tests.rs"]
mod tests;
