//! Which locale a session transcribes in: the user's choice, else the system locale when the
//! on-device engine has it installed, else `en-US`.

use std::path::Path;

use sotto_protocol::ui::is_bcp47_tag;

pub(crate) const FALLBACK_LOCALE: &str = "en-US";
const DEFAULTS: &str = "/usr/bin/defaults";

/// `pt_BR.UTF-8`, `pt_BR@rg=brzzzz` or `pt_BR` to `pt-BR`; anything that does not look like a
/// BCP 47 tag afterwards is dropped.
pub(crate) fn normalize(raw: &str) -> Option<String> {
    let base = raw.trim().split(['.', '@']).next().unwrap_or_default();
    let tag = base.replace('_', "-");
    (is_bcp47_tag(&tag) && tag != "C").then_some(tag)
}

/// The macOS region setting (`AppleLocale`), else the POSIX locale variables. `defaults` is run
/// by absolute path, never looked up on `PATH`.
#[must_use]
pub fn detect_system_locale() -> Option<String> {
    if Path::new(DEFAULTS).exists()
        && let Ok(out) = std::process::Command::new(DEFAULTS)
            .args(["read", "-g", "AppleLocale"])
            .output()
        && out.status.success()
        && let Some(tag) = normalize(&String::from_utf8_lossy(&out.stdout))
    {
        return Some(tag);
    }
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|k| std::env::var(k).ok())
        .find_map(|v| normalize(&v))
}

fn installed<'a>(tag: &str, list: &'a [String]) -> Option<&'a String> {
    list.iter().find(|l| l.eq_ignore_ascii_case(tag))
}

/// `chosen` wins while it is installed (or while the installed list is still unknown).
pub(crate) fn resolve(chosen: Option<&str>, system: Option<&str>, list: &[String]) -> String {
    if let Some(c) = chosen {
        if list.is_empty() {
            return c.to_string();
        }
        if let Some(l) = installed(c, list) {
            return l.clone();
        }
    }
    system
        .and_then(|s| installed(s, list))
        .cloned()
        .unwrap_or_else(|| FALLBACK_LOCALE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_posix_and_apple_forms() {
        assert_eq!(normalize("pt_BR.UTF-8").as_deref(), Some("pt-BR"));
        assert_eq!(normalize("en_US@rg=brzzzz\n").as_deref(), Some("en-US"));
        assert_eq!(normalize("pt-BR").as_deref(), Some("pt-BR"));
        assert_eq!(normalize("C"), None);
        assert_eq!(normalize(""), None);
    }

    #[test]
    fn system_locale_only_when_installed() {
        let list = vec!["en-US".to_string(), "pt-BR".to_string()];
        assert_eq!(resolve(None, Some("pt-br"), &list), "pt-BR");
        assert_eq!(resolve(None, Some("fr-FR"), &list), "en-US");
        assert_eq!(resolve(None, None, &list), "en-US");
        assert_eq!(resolve(Some("en-US"), Some("pt-BR"), &list), "en-US");
        assert_eq!(resolve(Some("de-DE"), Some("pt-BR"), &list), "pt-BR");
        assert_eq!(resolve(Some("de-DE"), Some("pt-BR"), &[]), "de-DE");
        assert_eq!(resolve(None, Some("pt-BR"), &[]), "en-US");
    }
}
