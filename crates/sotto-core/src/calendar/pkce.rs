//! PKCE (RFC 7636, S256) and the random `state` value.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use super::error::CalendarError;

const VERIFIER_BYTES: usize = 32;
const STATE_BYTES: usize = 24;

#[derive(Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce").finish_non_exhaustive()
    }
}

impl Pkce {
    pub fn generate() -> Result<Self, CalendarError> {
        let verifier = random_token(VERIFIER_BYTES)?;
        let challenge = challenge_for(&verifier);
        Ok(Self {
            verifier,
            challenge,
        })
    }
}

#[must_use]
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn state() -> Result<String, CalendarError> {
    random_token(STATE_BYTES)
}

fn random_token(bytes: usize) -> Result<String, CalendarError> {
    let mut buf = vec![0_u8; bytes];
    getrandom::fill(&mut buf).map_err(|_| CalendarError::Random)?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_the_rfc_7636_example() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifiers_are_unique_and_within_rfc_length() {
        let a = Pkce::generate().unwrap();
        let b = Pkce::generate().unwrap();
        assert_ne!(a.verifier, b.verifier);
        assert!((43..=128).contains(&a.verifier.len()));
        assert!(
            a.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_eq!(a.challenge, challenge_for(&a.verifier));
        assert_ne!(state().unwrap(), state().unwrap());
    }

    #[test]
    fn debug_hides_the_verifier() {
        let p = Pkce::generate().unwrap();
        assert!(!format!("{p:?}").contains(&p.verifier));
    }
}
