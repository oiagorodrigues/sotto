//! The backend origin and the `sotto://auth/callback` link, both validated before use.

use sotto_protocol::api::{AuthorizeQuery, CLIENT_ID, REDIRECT_URI};
use url::{Host, Url};

use super::error::AccountError;

const MAX_STATE_CHARS: usize = 128;
const MAX_CODE_CHARS: usize = 512;
const MAX_ERROR_CHARS: usize = 64;

/// The origin of the Sotto backend: https anywhere, plain http only for a loopback host (the
/// development server). No path, credentials, query or fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendUrl(Url);

impl BackendUrl {
    pub fn parse(raw: &str) -> Result<Self, AccountError> {
        let url = Url::parse(raw).map_err(|_| AccountError::BadBackendUrl("not a URL"))?;
        let host = url.host().ok_or(AccountError::BadBackendUrl("no host"))?;
        match url.scheme() {
            "https" => {}
            "http" => {
                let loopback = match host {
                    Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
                    Host::Ipv4(ip) => ip.is_loopback(),
                    Host::Ipv6(ip) => ip.is_loopback(),
                };
                if !loopback {
                    return Err(AccountError::BadBackendUrl(
                        "http is only allowed for localhost",
                    ));
                }
            }
            _ => return Err(AccountError::BadBackendUrl("only https is allowed")),
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(AccountError::BadBackendUrl(
                "only a scheme, host and port are allowed",
            ));
        }
        Ok(Self(url))
    }

    /// `scheme://host[:port]`, the form the shell's opener compares against.
    #[must_use]
    pub fn origin(&self) -> String {
        self.0.origin().ascii_serialization()
    }

    #[must_use]
    pub fn endpoint(&self, path: &str) -> String {
        let mut url = self.0.clone();
        url.set_path(path);
        url.into()
    }

    /// The login page the system browser opens.
    pub fn authorize_url(&self, state: &str, challenge: &str) -> Result<String, AccountError> {
        let query = AuthorizeQuery {
            response_type: "code".into(),
            client_id: CLIENT_ID.into(),
            redirect_uri: REDIRECT_URI.into(),
            state: state.into(),
            code_challenge: challenge.into(),
            code_challenge_method: "S256".into(),
        };
        let mut url = self.0.clone();
        url.set_path("/authorize");
        url.query_pairs_mut()
            .append_pair("response_type", &query.response_type)
            .append_pair("client_id", &query.client_id)
            .append_pair("redirect_uri", &query.redirect_uri)
            .append_pair("state", &query.state)
            .append_pair("code_challenge", &query.code_challenge)
            .append_pair("code_challenge_method", &query.code_challenge_method);
        Ok(url.into())
    }
}

/// What the backend redirected to. Codes and states are secrets until used, so `Debug` hides them.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Callback {
    pub state: String,
    /// The authorization code, or the OAuth `error` the backend reported.
    pub outcome: Result<String, String>,
}

impl std::fmt::Debug for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Callback").finish_non_exhaustive()
    }
}

/// `None` for anything that is not exactly a sign-in callback with a state and a code or error.
pub(super) fn parse_callback(raw: &str) -> Option<Callback> {
    let url = Url::parse(raw).ok()?;
    let redirect = Url::parse(REDIRECT_URI).ok()?;
    if url.scheme() != redirect.scheme()
        || url.host_str() != redirect.host_str()
        || url.path() != redirect.path()
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let (mut state, mut code, mut error) = (None, None, None);
    for (key, value) in url.query_pairs() {
        let slot = match key.as_ref() {
            "state" => &mut state,
            "code" => &mut code,
            "error" => &mut error,
            _ => continue,
        };
        if slot.replace(value.into_owned()).is_some() {
            return None;
        }
    }
    let state = state.filter(|s| !s.is_empty() && s.chars().count() <= MAX_STATE_CHARS)?;
    let outcome = match (code, error) {
        (Some(c), None) if !c.is_empty() && c.chars().count() <= MAX_CODE_CHARS => Ok(c),
        (None, Some(e)) if e.chars().count() <= MAX_ERROR_CHARS => Err(e),
        _ => return None,
    };
    Some(Callback { state, outcome })
}

/// Compares two secrets without stopping at the first different byte.
pub(super) fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0_u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_urls_allow_https_and_loopback_http_only() {
        for ok in [
            "https://api.sotto.example",
            "https://api.sotto.example:8443/",
            "http://localhost:8080",
            "http://127.0.0.1:8080/",
            "http://[::1]:8080",
        ] {
            assert!(BackendUrl::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://api.sotto.example",
            "http://localhost.evil.example",
            "ftp://localhost",
            "file:///etc/passwd",
            "https://user:pw@api.sotto.example",
            "https://api.sotto.example/v1",
            "https://api.sotto.example/?x=1",
            "https://api.sotto.example/#x",
            "sotto://auth",
            "localhost:8080",
            "",
        ] {
            assert!(BackendUrl::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_authorize_url_carries_the_contract_query() {
        let backend = BackendUrl::parse("http://localhost:8080").unwrap();
        let url = Url::parse(&backend.authorize_url("st-1", "chal").unwrap()).unwrap();
        assert_eq!(url.origin().ascii_serialization(), backend.origin());
        assert_eq!(url.path(), "/authorize");
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["redirect_uri"], REDIRECT_URI);
        assert_eq!(q["state"], "st-1");
        assert_eq!(q["code_challenge"], "chal");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q.len(), 6);
    }

    #[test]
    fn callbacks_parse_only_the_exact_link() {
        let ok = parse_callback("sotto://auth/callback?code=abc&state=xyz").unwrap();
        assert_eq!(ok.state, "xyz");
        assert_eq!(ok.outcome, Ok("abc".into()));
        let denied = parse_callback("sotto://auth/callback?error=access_denied&state=xyz").unwrap();
        assert_eq!(denied.outcome, Err("access_denied".into()));
        for bad in [
            "",
            "sotto://auth/callback",
            "sotto://auth/callback?code=abc",
            "sotto://auth/callback?state=xyz",
            "sotto://auth/callback?code=abc&state=",
            "sotto://auth/callback?code=abc&code=def&state=xyz",
            "sotto://auth/callback?code=abc&error=x&state=xyz",
            "sotto://auth/other?code=abc&state=xyz",
            "sotto://evil/callback?code=abc&state=xyz",
            "sotto://auth:99/callback?code=abc&state=xyz",
            "sotto://user@auth/callback?code=abc&state=xyz",
            "https://auth/callback?code=abc&state=xyz",
            "sotto://auth/callback/?code=abc&state=xyz",
        ] {
            assert!(parse_callback(bad).is_none(), "{bad}");
        }
        let long = format!("sotto://auth/callback?code={}&state=x", "a".repeat(513));
        assert!(parse_callback(&long).is_none());
    }

    #[test]
    fn callback_debug_hides_the_code_and_state() {
        let cb = parse_callback("sotto://auth/callback?code=SECRETCODE&state=SECRETSTATE").unwrap();
        let text = format!("{cb:?}");
        assert!(!text.contains("SECRET"));
    }

    #[test]
    fn constant_time_eq_compares_values() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(constant_time_eq("", ""));
    }
}
