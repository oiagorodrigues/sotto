//! The core's side of the Sotto account: forwards intents to the account task, reports the
//! settings that sync, applies settings another device changed, and mirrors the task's state into
//! the snapshot.

use sotto_protocol::ui::{AccountView, MAX_SPEND_CAP_MICRO_USD, is_bcp47_tag, is_known_model};
use tokio::sync::watch;

use super::{Core, settings, unix_ms};
use crate::account::{AccountHandle, Applies, Command, LocalSettings};

pub(super) enum AccountEvent {
    Changed,
    Apply(LocalSettings),
}

#[derive(Debug)]
pub(super) struct AccountLink {
    handle: AccountHandle,
    /// `None` once the task has stopped, so the select branch parks instead of spinning.
    changes: Option<watch::Receiver<AccountView>>,
    applies: Option<Applies>,
    reported: Option<LocalSettings>,
}

impl AccountLink {
    pub fn new(handle: AccountHandle, applies: Applies) -> Self {
        let changes = Some(handle.state());
        Self {
            handle,
            changes,
            applies: Some(applies),
            reported: None,
        }
    }

    pub fn send(&self, command: Command) {
        self.handle.send(command);
    }

    pub fn view(&self) -> AccountView {
        self.handle.state().borrow().clone()
    }

    /// Sends the settings to the task when they differ from the last ones sent.
    pub fn report(&mut self, settings: LocalSettings) {
        if self.reported.as_ref() != Some(&settings) {
            self.reported = Some(settings.clone());
            self.send(Command::Local(settings));
        }
    }

    pub async fn next(&mut self) -> AccountEvent {
        loop {
            if self.changes.is_none() && self.applies.is_none() {
                return std::future::pending().await;
            }
            tokio::select! {
                changed = async {
                    match &mut self.changes {
                        Some(rx) => rx.changed().await.is_ok(),
                        None => std::future::pending().await,
                    }
                } => {
                    if changed {
                        return AccountEvent::Changed;
                    }
                    self.changes = None;
                }
                applied = async {
                    match &mut self.applies {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => match applied {
                    Some(settings) => {
                        self.reported = Some(settings.clone());
                        return AccountEvent::Apply(settings);
                    }
                    None => self.applies = None,
                },
            }
        }
    }
}

impl Core {
    fn account_settings(&self) -> LocalSettings {
        LocalSettings {
            auto_answer_model: self.settings.auto_answer_model.clone(),
            manual_model: self.settings.manual_model.clone(),
            spend_cap_micro_usd: self.settings.guards.spend_cap_micro_usd,
            locale: self.providers.locale(&self.settings),
            auto_answer: self.settings.auto_answer,
            invisible: self.snap.invisible,
        }
    }

    pub(super) fn report_account_settings(&mut self) {
        let current = self.account_settings();
        self.account.report(current);
    }

    pub(super) fn refresh_account(&mut self) {
        let view = self.account.view();
        if view != self.snap.account {
            self.snap.account = view;
            self.dirty = true;
        }
    }

    pub(super) fn on_account(&mut self, event: AccountEvent) {
        match event {
            AccountEvent::Changed => {}
            AccountEvent::Apply(remote) => self.apply_remote_settings(&remote),
        }
        self.refresh_account();
    }

    /// Applies what another device changed. Each value is checked like the matching intent, so a
    /// document from the server can never put the app in a state the settings page cannot.
    fn apply_remote_settings(&mut self, remote: &LocalSettings) {
        let now = unix_ms();
        let auto = if is_known_model(&remote.auto_answer_model) {
            remote.auto_answer_model.clone()
        } else {
            self.settings.auto_answer_model.clone()
        };
        let manual = if is_known_model(&remote.manual_model) {
            remote.manual_model.clone()
        } else {
            self.settings.manual_model.clone()
        };
        if auto != self.settings.auto_answer_model || manual != self.settings.manual_model {
            self.settings.classifier_model =
                settings::classifier_for(&auto, &self.settings.classifier_model);
            self.settings.auto_answer_model = auto;
            self.settings.manual_model = manual;
            for (key, value) in settings::models_rows(
                &self.settings.auto_answer_model,
                &self.settings.manual_model,
            ) {
                self.store.set_setting(key, value, now);
            }
        }
        let cap = remote.spend_cap_micro_usd;
        if cap <= MAX_SPEND_CAP_MICRO_USD && cap != self.settings.guards.spend_cap_micro_usd {
            self.settings.guards.spend_cap_micro_usd = cap;
            if let Some(s) = self.session.as_mut() {
                s.guards.set_spend_cap(cap);
            }
            let (key, value) = settings::spend_cap_row(cap);
            self.store.set_setting(key, value, now);
        }
        if is_bcp47_tag(&remote.locale) && remote.locale != self.providers.locale(&self.settings) {
            let (key, value) = settings::locale_row(&remote.locale);
            self.store.set_setting(key, value, now);
            self.settings.locale = Some(remote.locale.clone());
        }
        if remote.auto_answer != self.settings.auto_answer {
            self.settings.auto_answer = remote.auto_answer;
            self.snap.auto_answer = remote.auto_answer;
            if !remote.auto_answer {
                self.discard_hidden();
            }
            let (key, value) = settings::auto_answer_row(remote.auto_answer);
            self.store.set_setting(key, value, now);
        }
        self.snap.invisible = remote.invisible;
        self.refresh_settings_view();
    }
}
