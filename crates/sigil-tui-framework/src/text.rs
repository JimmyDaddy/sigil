use std::fmt;

use sigil_tui_core::{CoreError, MAX_SURFACE_TEXT_BYTES};

/// Bounded, owned display text. It carries no domain or authority meaning.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Text(String);

impl Text {
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        if value.len() > MAX_SURFACE_TEXT_BYTES {
            return Err(CoreError::InvalidValue("text budget exceeded"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Display for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for Text {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for Text {
    type Error = CoreError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
