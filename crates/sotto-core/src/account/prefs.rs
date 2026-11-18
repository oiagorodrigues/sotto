//! The part of the synced document this device owns, and the merge rule.
//!
//! Settings are merged field by field: a field changed on this device after the server
//! document's `updated_at` keeps the local value (last writer wins), every other field takes the
//! server value. Shortcuts and modes are carried through from the server document untouched until
//! the core has local copies of them; [`compose`] is the one place to add them.

use std::collections::BTreeMap;

use sotto_protocol::api::{Preferences, SyncedSettings};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSettings {
    pub auto_answer_model: String,
    pub manual_model: String,
    pub spend_cap_micro_usd: u64,
    pub locale: String,
    pub auto_answer: bool,
    pub invisible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Field {
    AutoAnswerModel,
    ManualModel,
    SpendCap,
    Locale,
    AutoAnswer,
    Invisible,
}

pub(super) const FIELDS: [Field; 6] = [
    Field::AutoAnswerModel,
    Field::ManualModel,
    Field::SpendCap,
    Field::Locale,
    Field::AutoAnswer,
    Field::Invisible,
];

impl LocalSettings {
    pub(super) fn from_synced(s: &SyncedSettings) -> Self {
        Self {
            auto_answer_model: s.auto_answer_model.clone(),
            manual_model: s.manual_model.clone(),
            spend_cap_micro_usd: s.spend_cap_micro_usd,
            locale: s.locale.clone(),
            auto_answer: s.auto_answer,
            invisible: s.invisible,
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
        }
    }
}

/// Local edit times in milliseconds since the Unix epoch, per field.
pub(super) type Dirty = BTreeMap<Field, i64>;

/// The settings after merging the server document into the local ones. `server_updated_ms` is
/// `None` when the server has no timestamp, in which case the server wins every field.
pub(super) fn merge(
    local: &LocalSettings,
    dirty: &Dirty,
    server: &SyncedSettings,
    server_updated_ms: Option<i64>,
) -> LocalSettings {
    let server = LocalSettings::from_synced(server);
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
    merged
}

/// The document to push: the server's shortcuts, modes and engine preference with this device's
/// settings on top.
pub(super) fn compose(server: Option<&Preferences>, local: &LocalSettings) -> Preferences {
    let (engine_preference, shortcuts, modes) = match server {
        Some(p) => (
            p.settings.engine_preference.clone(),
            p.shortcuts.clone(),
            p.modes.clone(),
        ),
        None => ("local".to_string(), BTreeMap::new(), Vec::new()),
    };
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

    fn local() -> LocalSettings {
        LocalSettings {
            auto_answer_model: "claude-haiku-5-5".into(),
            manual_model: "claude-sonnet-5-5".into(),
            spend_cap_micro_usd: 500_000,
            locale: "en-US".into(),
            auto_answer: true,
            invisible: true,
        }
    }

    fn server() -> SyncedSettings {
        SyncedSettings {
            auto_answer_model: "gpt-5-mini".into(),
            manual_model: "gpt-5".into(),
            spend_cap_micro_usd: 1_000_000,
            locale: "pt-BR".into(),
            auto_answer: false,
            invisible: false,
            engine_preference: "cloud".into(),
        }
    }

    #[test]
    fn untouched_fields_take_the_server_value() {
        let merged = merge(&local(), &Dirty::new(), &server(), Some(1_000));
        assert_eq!(merged, LocalSettings::from_synced(&server()));
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
    fn compose_carries_server_extras_and_defaults_without_a_document() {
        let mut doc = compose(None, &local());
        assert_eq!(doc.settings.engine_preference, "local");
        assert!(doc.shortcuts.is_empty() && doc.modes.is_empty());
        doc.shortcuts.insert("toggle".into(), "Cmd+K".into());
        doc.settings.engine_preference = "auto".into();
        let again = compose(Some(&doc), &local());
        assert_eq!(again, doc);
    }
}
