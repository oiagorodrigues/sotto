//! File logging. Logs carry events and error kinds only, never transcript text, prompts, answers
//! or keys (see the rules in CLAUDE.md).

use std::fs;
use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

const LOG_FILE: &str = "sotto.log";
const OLD_LOG_FILE: &str = "sotto.log.1";
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Moves a log past the size cap aside, so the log directory holds at most two files of that size.
fn rotate(dir: &Path) {
    let current = dir.join(LOG_FILE);
    let too_big = fs::metadata(&current).is_ok_and(|m| m.len() > MAX_LOG_BYTES);
    if too_big {
        let _ = fs::rename(&current, dir.join(OLD_LOG_FILE));
    }
}

/// Installs the global subscriber writing to `<dir>/sotto.log` at info level (`RUST_LOG`
/// overrides). The returned guard flushes the file on drop and must outlive the app.
pub fn init(dir: &Path) -> Option<WorkerGuard> {
    fs::create_dir_all(dir).ok()?;
    rotate(dir);
    let (writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::never(dir, LOG_FILE));
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init()
        .ok()?;
    Some(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_past_the_cap_is_moved_aside() {
        let dir = std::env::temp_dir().join(format!("sotto-log-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let big = vec![b'x'; usize::try_from(MAX_LOG_BYTES).unwrap() + 1];
        fs::write(dir.join(LOG_FILE), big).unwrap();
        rotate(&dir);
        assert!(!dir.join(LOG_FILE).exists());
        assert!(dir.join(OLD_LOG_FILE).exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
