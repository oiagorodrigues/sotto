//! Persistence and locale rules for the transcription engine choice.

use sotto_protocol::ui::EnginePreference;

use super::Settings;
use super::locale::FALLBACK_LOCALE;
use crate::storage::Db;

const KEY_ENGINE: &str = "engine_preference";

pub(crate) fn engine_row(engine: EnginePreference) -> (&'static str, serde_json::Value) {
    let value = match engine {
        EnginePreference::Local => "local",
        EnginePreference::Cloud => "cloud",
    };
    (KEY_ENGINE, value.into())
}

/// Unknown or unreadable values keep the default, which is the on-device engine.
pub(crate) fn apply_saved(settings: &mut Settings, db: &Db) {
    let saved = db.get_setting(KEY_ENGINE).ok().flatten();
    match saved.as_ref().and_then(serde_json::Value::as_str) {
        Some("cloud") => settings.engine_preference = EnginePreference::Cloud,
        Some("local") => settings.engine_preference = EnginePreference::Local,
        _ => {}
    }
}

/// The locale a cloud session transcribes in: the user's choice, else the system locale, else
/// `en-US`. The cloud engine is not limited to the languages installed on the Mac. `None` when
/// the preference is the on-device engine.
pub(crate) fn cloud_locale(settings: &Settings) -> Option<String> {
    (settings.engine_preference == EnginePreference::Cloud).then(|| {
        settings
            .locale
            .clone()
            .or_else(|| settings.system_locale.clone())
            .unwrap_or_else(|| FALLBACK_LOCALE.to_string())
    })
}
