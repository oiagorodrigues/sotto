//! The login the app started: kept in memory for this process only (ADR 0008).

use tokio::time::{Duration, Instant};

use super::error::AccountError;
use super::url::constant_time_eq;
use crate::calendar::{Pkce, random_state};

pub(super) struct Pending {
    state: String,
    verifier: String,
    expires_at: Instant,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending").finish_non_exhaustive()
    }
}

impl Pending {
    /// Returns the pending login and the PKCE challenge to send to `/authorize`.
    pub fn start(ttl: Duration) -> Result<(Self, String, String), AccountError> {
        let pkce = Pkce::generate().map_err(|_| AccountError::Random)?;
        let state = random_state().map_err(|_| AccountError::Random)?;
        let pending = Self {
            state: state.clone(),
            verifier: pkce.verifier,
            expires_at: Instant::now() + ttl,
        };
        Ok((pending, state, pkce.challenge))
    }

    pub fn matches(&self, state: &str) -> bool {
        constant_time_eq(&self.state, state)
    }

    pub fn expires_at(&self) -> Instant {
        self.expires_at
    }

    pub fn expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }

    pub fn into_verifier(self) -> String {
        self.verifier
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_its_own_state() {
        let (p, state, _) = Pending::start(Duration::from_secs(300)).unwrap();
        assert!(p.matches(&state));
        assert!(!p.matches("other"));
        assert!(!p.matches(""));
        assert!(!p.matches(&format!("{state}x")));
    }

    #[test]
    fn expires_after_the_ttl() {
        let (p, _, _) = Pending::start(Duration::ZERO).unwrap();
        assert!(p.expired());
        let (p, _, _) = Pending::start(Duration::from_secs(300)).unwrap();
        assert!(!p.expired());
    }

    #[test]
    fn debug_hides_state_and_verifier() {
        let (p, state, _) = Pending::start(Duration::from_secs(300)).unwrap();
        let text = format!("{p:?}");
        assert!(!text.contains(&state));
        assert!(!text.contains(&p.verifier));
    }
}
