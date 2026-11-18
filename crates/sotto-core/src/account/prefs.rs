//! The part of the synced document this device owns, and the merge rule.
//!
//! Settings are merged field by field: a field changed on this device after the server
//! document's `updated_at` keeps the local value (last writer wins), every other field takes the
//! server value. Modes are merged one by one, the newer edit of each id winning, and deletions
//! travel as tombstones. `engine_preference` is deliberately not synced (ADR 0003: cloud
//! transcription is an explicit opt-in on each device), and shortcuts are carried through from
//! the server document until the core has local copies of them; [`compose`] is the one place to
//! add them.

use std::collections::BTreeMap;

use sotto_protocol::api::{Preferences, SyncedMode, SyncedSettings};

/// A deleted mode stops travelling in the document after this long.
pub(crate) const TOMBSTONE_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// A mode as the core and the sync task exchange it: name and system prompt only, never the
/// attached files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMode {
    pub id: String,
    pub name: String,
    pub system_prompt: String,
    /// Milliseconds since the Unix epoch of the last edit, or of the deletion.
    pub updated_ms: i64,
    pub deleted: bool,
}

impl LocalMode {
    fn from_synced(m: &SyncedMode) -> Self {
        Self {
            id: m.id.clone(),
            name: m.name.clone(),
            system_prompt: m.system_prompt.clone(),
            updated_ms: parse_ms(&m.updated_at).unwrap_or(0),
            deleted: m.deleted,
        }
    }

    fn to_synced(&self) -> SyncedMode {
        SyncedMode {
            id: self.id.clone(),
            name: self.name.clone(),
            system_prompt: self.system_prompt.clone(),
            updated_at: format_ms(self.updated_ms),
            deleted: self.deleted,
        }
    }
}

pub(super) fn parse_ms(rfc3339: &str) -> Option<i64> {
    let t = time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339)
        .ok()?;
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()
}

fn format_ms(ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

/// The union of both lists by id, the newer edit of each id winning (the server on a tie), sorted
/// by id.
fn merge_modes(local: &[LocalMode], server: &[LocalMode]) -> Vec<LocalMode> {
    let mut by_id: BTreeMap<&str, &LocalMode> = server.iter().map(|m| (m.id.as_str(), m)).collect();
    for m in local {
        match by_id.get(m.id.as_str()) {
            Some(theirs) if theirs.updated_ms >= m.updated_ms => {}
            _ => {
                by_id.insert(m.id.as_str(), m);
            }
        }
    }
    by_id.into_values().cloned().collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSettings {
    pub auto_answer_model: String,
    pub manual_model: String,
    pub spend_cap_micro_usd: u64,
    pub locale: String,
    pub auto_answer: bool,
    pub invisible: bool,
    /// Sorted by id, tombstones included.
    pub modes: Vec<LocalMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Field {
    AutoAnswerModel,
    ManualModel,
    SpendCap,
    Locale,
    AutoAnswer,
    Invisible,
    Modes,
}

pub(super) const FIELDS: [Field; 7] = [
    Field::AutoAnswerModel,
    Field::ManualModel,
    Field::SpendCap,
    Field::Locale,
    Field::AutoAnswer,
    Field::Invisible,
    Field::Modes,
];

impl LocalSettings {
    pub(super) fn from_preferences(p: &Preferences) -> Self {
        let s = &p.settings;
        let mut modes: Vec<LocalMode> = p.modes.iter().map(LocalMode::from_synced).collect();
        modes.sort_by(|a, b| a.id.cmp(&b.id));
        Self {
            auto_answer_model: s.auto_answer_model.clone(),
            manual_model: s.manual_model.clone(),
            spend_cap_micro_usd: s.spend_cap_micro_usd,
            locale: s.locale.clone(),
            auto_answer: s.auto_answer,
            invisible: s.invisible,
            modes,
        }
    }

    pub(super) fn differs(&self, other: &Self, field: Field) -> bool {
        match field {
            Field::AutoAnswerModel => self.auto_answer_model != other.auto_answer_model,
            Field::ManualModel => self.manual_model != other.manual_model,
            Field::SpendCap => self.spend_cap_micro_usd != other.spend_cap_micro_usd,
            Field::Locale => self.locale != other.locale,
            Field::AutoAnswer => self.auto_answer != other.auto_answer,
            Field::Invisible => self.invisible != other.invisible,
            Field::Modes => self.modes != other.modes,
        }
    }

    fn copy_field(&mut self, from: &Self, field: Field) {
        match field {
            Field::AutoAnswerModel => self.auto_answer_model.clone_from(&from.auto_answer_model),
            Field::ManualModel => self.manual_model.clone_from(&from.manual_model),
            Field::SpendCap => self.spend_cap_micro_usd = from.spend_cap_micro_usd,
            Field::Locale => self.locale.clone_from(&from.locale),
            Field::AutoAnswer => self.auto_answer = from.auto_answer,
            Field::Invisible => self.invisible = from.invisible,
            Field::Modes => {}
        }
    }
}

/// Local edit times in milliseconds since the Unix epoch, per field.
pub(super) type Dirty = BTreeMap<Field, i64>;

/// The settings after merging the server document into the local ones. `server_updated_ms` is
/// `None` when the server has no timestamp, in which case the server wins every setting. Modes
/// do not depend on it: each mode carries its own edit time.
pub(super) fn merge(
    local: &LocalSettings,
    dirty: &Dirty,
    server: &Preferences,
    server_updated_ms: Option<i64>,
) -> LocalSettings {
    let server = LocalSettings::from_preferences(server);
    let mut merged = server.clone();
    for field in FIELDS {
        let kept = match (dirty.get(&field), server_updated_ms) {
            (Some(edited), Some(updated)) => *edited > updated,
            _ => false,
        };
        if kept {
            merged.copy_field(local, field);
        }
    }
    merged.modes = merge_modes(&local.modes, &server.modes);
    merged
}

/// The document to push: this device's settings and modes on top of the server's shortcuts and
/// of the modes this device does not know. `engine_preference` is the server's value untouched
/// (`"local"` for a new account) because it is never applied here.
pub(super) fn compose(
    server: Option<&Preferences>,
    local: &LocalSettings,
    now_ms: i64,
) -> Preferences {
    let (engine_preference, shortcuts, server_modes) = match server {
        Some(p) => (
            p.settings.engine_preference.clone(),
            p.shortcuts.clone(),
            LocalSettings::from_preferences(p).modes,
        ),
        None => ("local".to_string(), BTreeMap::new(), Vec::new()),
    };
    let modes = merge_modes(&local.modes, &server_modes)
        .iter()
        .filter(|m| !m.deleted || now_ms - m.updated_ms < TOMBSTONE_TTL_MS)
        .map(LocalMode::to_synced)
        .collect();
    Preferences {
        settings: SyncedSettings {
            auto_answer_model: local.auto_answer_model.clone(),
            manual_model: local.manual_model.clone(),
            spend_cap_micro_usd: local.spend_cap_micro_usd,
            locale: local.locale.clone(),
            auto_answer: local.auto_answer,
            invisible: local.invisible,
            engine_preference,
        },
        shortcuts,
        modes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(id: &str, prompt: &str, updated_ms: i64) -> LocalMode {
        LocalMode {
            id: id.into(),
            name: format!("mode {id}"),
            system_prompt: prompt.into(),
            updated_ms,
            deleted: false,
        }
    }

    fn local() -> LocalSettings {
        LocalSettings {
            auto_answer_model: "claude-haiku-5-5".into(),
            manual_model: "claude-sonnet-5-5".into(),
            spend_cap_micro_usd: 500_000,
            locale: "en-US".into(),
            auto_answer: true,
            invisible: true,
            modes: Vec::new(),
        }
    }

    fn server() -> Preferences {
        Preferences {
            settings: SyncedSettings {
                auto_answer_model: "gpt-5-mini".into(),
                manual_model: "gpt-5".into(),
                spend_cap_micro_usd: 1_000_000,
                locale: "pt-BR".into(),
                auto_answer: false,
                invisible: false,
                engine_preference: "cloud".into(),
            },
            shortcuts: BTreeMap::new(),
            modes: Vec::new(),
        }
    }

    #[test]
    fn untouched_fields_take_the_server_value() {
        let merged = merge(&local(), &Dirty::new(), &server(), Some(1_000));
        assert_eq!(merged, LocalSettings::from_preferences(&server()));
    }

    #[test]
    fn a_field_edited_after_the_server_write_keeps_the_local_value() {
        let dirty = Dirty::from([(Field::Locale, 2_000), (Field::SpendCap, 500)]);
        let merged = merge(&local(), &dirty, &server(), Some(1_000));
        assert_eq!(merged.locale, "en-US");
        assert_eq!(merged.spend_cap_micro_usd, 1_000_000);
        assert_eq!(merged.manual_model, "gpt-5");
    }

    #[test]
    fn the_server_wins_without_a_timestamp() {
        let dirty = Dirty::from([(Field::Locale, 2_000)]);
        let merged = merge(&local(), &dirty, &server(), None);
        assert_eq!(merged.locale, "pt-BR");
    }

    #[test]
    fn modes_merge_one_by_one_and_the_newer_edit_wins() {
        let mut doc = server();
        doc.modes = vec![
            mode("a", "server a", 5_000).to_synced(),
            mode("b", "server b", 1_000).to_synced(),
        ];
        let mut mine = local();
        mine.modes = vec![
            mode("a", "local a", 4_000),
            mode("b", "local b", 2_000),
            mode("c", "local c", 3_000),
        ];
        let merged = merge(&mine, &Dirty::new(), &doc, Some(9_000));
        let text: Vec<_> = merged
            .modes
            .iter()
            .map(|m| m.system_prompt.as_str())
            .collect();
        assert_eq!(text, ["server a", "local b", "local c"]);
    }

    #[test]
    fn a_newer_tombstone_deletes_and_an_older_one_does_not() {
        let mut gone = mode("a", "", 5_000);
        gone.deleted = true;
        let mut doc = server();
        doc.modes = vec![gone.to_synced()];
        let mut mine = local();
        mine.modes = vec![mode("a", "kept", 4_000)];
        assert!(merge(&mine, &Dirty::new(), &doc, None).modes[0].deleted);
        mine.modes = vec![mode("a", "kept", 6_000)];
        assert!(!merge(&mine, &Dirty::new(), &doc, None).modes[0].deleted);
    }

    #[test]
    fn compose_never_pushes_a_cloud_preference_and_keeps_unknown_server_modes() {
        let mut doc = compose(None, &local(), 0);
        assert_eq!(doc.settings.engine_preference, "local");
        assert!(doc.shortcuts.is_empty() && doc.modes.is_empty());
        doc.shortcuts.insert("toggle".into(), "Cmd+K".into());
        doc.settings.engine_preference = "auto".into();
        doc.modes = vec![mode("only-on-server", "p", 1).to_synced()];
        let again = compose(Some(&doc), &local(), 0);
        assert_eq!(again, doc);
        assert_eq!(again.settings.engine_preference, "auto");
    }

    #[test]
    fn compose_drops_old_tombstones_and_formats_times() {
        let mut old = mode("old", "", 1_000);
        old.deleted = true;
        let mut recent = mode("recent", "", TOMBSTONE_TTL_MS);
        recent.deleted = true;
        let mut mine = local();
        mine.modes = vec![old, recent, mode("live", "p", 1_767_225_600_000)];
        let doc = compose(None, &mine, TOMBSTONE_TTL_MS + 5_000);
        let ids: Vec<_> = doc.modes.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["live", "recent"]);
        assert_eq!(doc.modes[0].updated_at, "2026-01-01T00:00:00Z");
        assert_eq!(parse_ms(&doc.modes[0].updated_at), Some(1_767_225_600_000));
    }
}
