use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::{Zeroize, Zeroizing};

use crate::launcher::DesktopLaunchError;

pub(crate) struct DesktopBearerToken(Zeroizing<String>);

impl DesktopBearerToken {
    pub(crate) fn generate() -> Result<Self, DesktopLaunchError> {
        let mut bytes = [0_u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| DesktopLaunchError::BearerGenerationFailed)?;
        let encoded = URL_SAFE_NO_PAD.encode(bytes);
        bytes.zeroize();
        Ok(Self(Zeroizing::new(encoded)))
    }

    pub(crate) fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for DesktopBearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DesktopBearerToken(<redacted>)")
    }
}

#[cfg(test)]
#[path = "tests/secret_tests.rs"]
mod tests;
