use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize};

const CONNECTION_ID_MAX_BYTES: usize = 64;
const MODEL_ID_MAX_BYTES: usize = 256;
const ROUTE_LABEL_MAX_BYTES: usize = 64;
const ROUTE_FINGERPRINT_MAX_BYTES: usize = 128;

/// Stable provider-neutral identity for one saved provider connection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ConnectionId(String);

impl ConnectionId {
    /// Validates and constructs a connection identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is empty, exceeds 64 bytes, or contains characters outside
    /// the bounded ASCII identity alphabet.
    pub fn new(value: impl Into<String>) -> Result<Self, ModelRouteValidationError> {
        let value = value.into();
        validate_connection_id(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ConnectionId {
    type Err = ModelRouteValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for ConnectionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Exact model identity scoped to one provider connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ModelRef {
    pub connection_id: ConnectionId,
    pub model_id: String,
}

impl ModelRef {
    /// Constructs a compound model identity after admitting the provider model ID.
    ///
    /// # Errors
    ///
    /// Returns an error when the model ID is empty, oversized, or terminal-unsafe.
    pub fn new(
        connection_id: ConnectionId,
        model_id: impl Into<String>,
    ) -> Result<Self, ModelRouteValidationError> {
        let model_id = model_id.into();
        let model_id = model_id.trim();
        validate_model_id(model_id)?;
        Ok(Self {
            connection_id,
            model_id: model_id.to_owned(),
        })
    }
}

impl<'de> Deserialize<'de> for ModelRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireModelRef {
            connection_id: ConnectionId,
            model_id: String,
        }

        let value = WireModelRef::deserialize(deserializer)?;
        Self::new(value.connection_id, value.model_id).map_err(serde::de::Error::custom)
    }
}

/// Secret-free route identity frozen when a session is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedModelRoute {
    pub model_ref: ModelRef,
    pub provider_family: String,
    pub protocol: String,
    pub semantic_fingerprint: String,
}

impl ResolvedModelRoute {
    /// Constructs an admitted durable route identity.
    ///
    /// # Errors
    ///
    /// Returns an error when a provider/protocol label or fingerprint is empty, oversized, or
    /// contains control characters.
    pub fn new(
        model_ref: ModelRef,
        provider_family: impl Into<String>,
        protocol: impl Into<String>,
        semantic_fingerprint: impl Into<String>,
    ) -> Result<Self, ModelRouteValidationError> {
        let provider_family = provider_family.into();
        let protocol = protocol.into();
        let semantic_fingerprint = semantic_fingerprint.into();
        validate_route_label("provider family", &provider_family)?;
        validate_route_label("protocol", &protocol)?;
        validate_safe_text(
            "semantic fingerprint",
            &semantic_fingerprint,
            ROUTE_FINGERPRINT_MAX_BYTES,
        )?;
        Ok(Self {
            model_ref,
            provider_family,
            protocol,
            semantic_fingerprint,
        })
    }
}

impl<'de> Deserialize<'de> for ResolvedModelRoute {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireResolvedModelRoute {
            model_ref: ModelRef,
            provider_family: String,
            protocol: String,
            semantic_fingerprint: String,
        }

        let value = WireResolvedModelRoute::deserialize(deserializer)?;
        Self::new(
            value.model_ref,
            value.provider_family,
            value.protocol,
            value.semantic_fingerprint,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelRouteValidationError {
    #[error("{field} must contain 1..={max_bytes} bytes")]
    InvalidLength {
        field: &'static str,
        max_bytes: usize,
    },
    #[error("{field} contains an invalid character")]
    InvalidCharacter { field: &'static str },
}

fn validate_connection_id(value: &str) -> Result<(), ModelRouteValidationError> {
    if value.is_empty() || value.len() > CONNECTION_ID_MAX_BYTES {
        return Err(ModelRouteValidationError::InvalidLength {
            field: "connection id",
            max_bytes: CONNECTION_ID_MAX_BYTES,
        });
    }
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return Err(ModelRouteValidationError::InvalidLength {
            field: "connection id",
            max_bytes: CONNECTION_ID_MAX_BYTES,
        });
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(ModelRouteValidationError::InvalidCharacter {
            field: "connection id",
        });
    }
    if !bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    }) {
        return Err(ModelRouteValidationError::InvalidCharacter {
            field: "connection id",
        });
    }
    Ok(())
}

fn validate_model_id(value: &str) -> Result<(), ModelRouteValidationError> {
    validate_safe_text("model id", value, MODEL_ID_MAX_BYTES)
}

fn validate_route_label(field: &'static str, value: &str) -> Result<(), ModelRouteValidationError> {
    if value.is_empty() || value.len() > ROUTE_LABEL_MAX_BYTES {
        return Err(ModelRouteValidationError::InvalidLength {
            field,
            max_bytes: ROUTE_LABEL_MAX_BYTES,
        });
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    }) {
        return Err(ModelRouteValidationError::InvalidCharacter { field });
    }
    Ok(())
}

fn validate_safe_text(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ModelRouteValidationError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(ModelRouteValidationError::InvalidLength { field, max_bytes });
    }
    if value.chars().any(is_unsafe_route_character) {
        return Err(ModelRouteValidationError::InvalidCharacter { field });
    }
    Ok(())
}

fn is_unsafe_route_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{009b}'
                | '\u{009d}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

#[cfg(test)]
#[path = "tests/model_route_tests.rs"]
mod tests;
