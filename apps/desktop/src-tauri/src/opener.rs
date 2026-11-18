//! Opens the Google sign-in page in the default browser. The webview cannot do this: the
//! capabilities allow no shell or opener plugin (config_tests.rs).

use sotto_core::calendar::UrlOpener;

const OPEN: &str = "/usr/bin/open";

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemBrowser;

impl UrlOpener for SystemBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        if !url.starts_with("https://") {
            return Err("only https links are opened".into());
        }
        open_in_default_browser(url)
    }
}

/// Callers check the URL first; it must start with a scheme so `open` never reads it as a flag.
pub fn open_in_default_browser(url: &str) -> Result<(), String> {
    let status = std::process::Command::new(OPEN)
        .arg(url)
        .status()
        .map_err(|e| e.kind().to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("the system could not open the link".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_anything_but_https() {
        for url in [
            "http://example.com",
            "file:///etc/passwd",
            "-a Calculator",
            "",
        ] {
            assert!(SystemBrowser.open(url).is_err(), "{url}");
        }
    }
}
