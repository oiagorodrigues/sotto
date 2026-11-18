//! Where the Sotto backend lives, the opener that may only open its login page, and the filter
//! for `sotto://` links handed to the core (ADR 0008).

use tauri::Url;

/// True for `sotto://auth/callback…`. The core still checks the query against the login it
/// started; this only keeps other links from reaching it.
pub fn is_auth_callback(url: &Url) -> bool {
    url.scheme() == "sotto" && url.host_str() == Some("auth") && url.path() == "/callback"
}

#[cfg(not(feature = "demo"))]
pub use self::real::{BackendBrowser, backend_url};

#[cfg(not(feature = "demo"))]
mod real {
    use sotto_core::calendar::UrlOpener;
    use tauri::Url;

    /// Set at build time (`SOTTO_API_URL=https://… pnpm tauri build`). Debug builds fall back to
    /// the local dev server; a release build without it has no sign-in. It is not read at run
    /// time, so the environment of a running app cannot point the login at another server.
    const CONFIGURED_URL: Option<&str> = option_env!("SOTTO_API_URL");
    const DEV_URL: &str = "http://localhost:8080";
    const LOGIN_PATH: &str = "/authorize";

    pub fn backend_url() -> Option<&'static str> {
        match CONFIGURED_URL {
            Some(url) if !url.is_empty() => Some(url),
            _ if cfg!(debug_assertions) => Some(DEV_URL),
            _ => None,
        }
    }

    /// Opens the login page of one backend origin and nothing else.
    #[derive(Debug, Clone)]
    pub struct BackendBrowser {
        origin: String,
    }

    impl BackendBrowser {
        pub fn new(origin: String) -> Self {
            Self { origin }
        }

        fn allows(&self, raw: &str) -> bool {
            Url::parse(raw).is_ok_and(|url| {
                url.origin().ascii_serialization() == self.origin
                    && url.path() == LOGIN_PATH
                    && url.username().is_empty()
                    && url.password().is_none()
            })
        }
    }

    impl UrlOpener for BackendBrowser {
        fn open(&self, url: &str) -> Result<(), String> {
            if !self.allows(url) {
                return Err("only the Sotto login page is opened".into());
            }
            crate::opener::open_in_default_browser(url)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_the_configured_origin_and_login_path_open() {
            let browser = BackendBrowser::new("http://localhost:8080".into());
            assert!(browser.allows("http://localhost:8080/authorize?state=x"));
            for bad in [
                "http://localhost:8081/authorize",
                "https://localhost:8080/authorize",
                "http://localhost:8080.evil.example/authorize",
                "http://localhost:8080/other",
                "http://user@localhost:8080/authorize",
                "https://accounts.google.com/o/oauth2/v2/auth",
                "file:///etc/passwd",
                "-a Calculator",
                "",
            ] {
                assert!(!browser.allows(bad), "{bad}");
                assert!(browser.open(bad).is_err(), "{bad}");
            }
        }

        #[test]
        fn debug_builds_default_to_the_dev_server() {
            if cfg!(debug_assertions) && CONFIGURED_URL.is_none() {
                assert_eq!(backend_url(), Some("http://localhost:8080"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_auth_callback_link_is_forwarded() {
        let url = |s: &str| Url::parse(s).unwrap();
        assert!(is_auth_callback(&url(
            "sotto://auth/callback?code=a&state=b"
        )));
        for other in [
            "sotto://auth/other",
            "sotto://settings/callback",
            "https://auth/callback",
            "sotto://auth/callback/extra",
        ] {
            assert!(!is_auth_callback(&url(other)), "{other}");
        }
    }
}
