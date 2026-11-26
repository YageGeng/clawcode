use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::time::Instant;
use typed_builder::TypedBuilder;

pub(crate) const ALPN: &[u8] = b"clawcode/acp/1";
const VERSION: &str = "1";
const VALIDITY: Duration = Duration::from_secs(300);
const MAX_MISTAKES: u8 = 3;

/// Text and QR payload with an absolute expiry for the host's countdown.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Invitation {
    pub code: String,
    pub expires_at: u64,
    /// Nonsecret identifier used to retire a displayed invitation without exposing its payload.
    pub generation: String,
}

/// The payload includes contact hints, but peer identity always comes from QUIC TLS.
#[derive(Deserialize, Serialize)]
pub(crate) struct Ticket {
    pub(crate) addr: EndpointAddr,
    pub(crate) secret: String,
    expires_at: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Hello {
    Pair { secret: String, name: String },
    Connect,
}

#[derive(TypedBuilder)]
pub(crate) struct Challenge {
    pub(crate) generation: String,
    token: String,
    pub(crate) deadline: Instant,
    #[builder(default)]
    mistakes: u8,
    expires_at: u64,
}

impl Challenge {
    /// Generate 192 random bits, independent of the persisted device signing key.
    pub(crate) fn issue(addr: EndpointAddr) -> Result<(Self, Invitation)> {
        // Generate a public display identifier independently of the secret bits.
        let mut entropy = [0u8; 40];
        getrandom::fill(&mut entropy)
            .context("Could not generate pairing secret")?;
        let token = URL_SAFE_NO_PAD.encode(&entropy[..24]);
        let generation = URL_SAFE_NO_PAD.encode(&entropy[24..]);
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
        let expires_at = now.as_secs() + VALIDITY.as_secs();
        let code = format!(
            "claw{VERSION}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&Ticket {
                addr,
                secret: token.clone(),
                expires_at,
            })?)
        );
        let challenge = Self::builder()
            .generation(generation.clone())
            .token(token)
            .deadline(
                Instant::now()
                    + Duration::from_secs(expires_at).saturating_sub(now),
            )
            .expires_at(expires_at)
            .build();
        Ok((
            challenge,
            Invitation {
                code,
                expires_at,
                generation,
            },
        ))
    }

    /// Return whether a controller can still claim this generation.
    pub(crate) fn active(&self) -> bool {
        self.mistakes < MAX_MISTAKES
            && Instant::now() < self.deadline
            && SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .is_ok_and(|now| now.as_secs() < self.expires_at)
    }

    /// Consume a single successful attempt, with a bounded budget shared by all peers.
    pub(crate) fn claim(&mut self, candidate: &str) -> Result<Instant> {
        ensure!(
            Instant::now() < self.deadline
                && SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()
                    < self.expires_at,
            "Pairing code has expired"
        );
        ensure!(
            self.mistakes < MAX_MISTAKES,
            "Pairing code is no longer valid"
        );
        if !bool::from(self.token.as_bytes().ct_eq(candidate.as_bytes())) {
            self.mistakes += 1;
            anyhow::bail!("Invalid pairing code");
        }
        self.mistakes = MAX_MISTAKES;
        Ok(self.deadline)
    }
}

impl Ticket {
    /// Decode a bounded payload, rejecting expired codes before any network activity.
    pub(crate) fn parse(code: &str) -> Result<Self> {
        let code = code.trim();
        ensure!(code.len() <= 8192, "Invalid pairing code");
        let (version, payload) = code
            .strip_prefix("claw")
            .and_then(|text| text.split_once('.'))
            .context("Invalid pairing code")?;
        ensure!(
            !version.is_empty()
                && version.bytes().all(|byte| byte.is_ascii_digit())
                && !payload.is_empty(),
            "Invalid pairing code"
        );
        ensure!(
            version == VERSION,
            "Incompatible pairing code version; upgrade both devices"
        );
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .context("Invalid pairing code")?;
        let ticket: Self = serde_json::from_slice(&bytes).map_err(
            |_private_payload_error| anyhow::anyhow!("Invalid pairing code"),
        )?;
        ensure!(ticket.secret.len() == 32, "Invalid pairing code");
        ensure!(
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()
                < ticket.expires_at,
            "Pairing code has expired"
        );
        Ok(ticket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    /// Classify unsupported format versions separately from unrelated clipboard contents.
    #[test]
    fn unrelated_text_is_invalid_and_numeric_versions_have_upgrade_guidance()
    -> Result<()> {
        for (code, expected) in [
            ("hello", "Invalid pairing code"),
            ("clawX.blob", "Invalid pairing code"),
            ("claw2.blob", "upgrade"),
        ] {
            let error = Ticket::parse(code)
                .err()
                .context("Invalid text was accepted")?;
            ensure!(
                error.to_string().contains(expected),
                "Unexpected validation message: {error}"
            );
        }
        Ok(())
    }

    /// Ensure modifying the invitation's advertised expiry cannot extend the host's monotonic TTL.
    #[test]
    fn expired_host_challenge_rejects_the_original_secret() -> Result<()> {
        let addr = EndpointAddr::new(SecretKey::generate().public());
        let (mut challenge, invitation) = Challenge::issue(addr)?;
        let ticket = Ticket::parse(&invitation.code)?;
        challenge.deadline = Instant::now();
        challenge
            .claim(&ticket.secret)
            .expect_err("Expired secret was accepted");
        Ok(())
    }
}
