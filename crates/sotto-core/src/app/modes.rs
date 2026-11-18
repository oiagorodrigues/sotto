//! Modes: a name, a system prompt and attached files. This state lives next to the event loop
//! (the loop owns it and calls in), keeps only metadata in memory, and does all file, database
//! and embedding work in tasks that report back as [`ModesMsg`] (ADR 0011).

use std::path::PathBuf;
use std::sync::Arc;

use sotto_protocol::ui::{Intent, ModeFileStatus, ModeFileView, ModeView};
use sotto_rag::{Embedder, estimate_tokens};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::event_loop::unix_ms;
use super::mode_context::{FULL_TEXT_MAX_TOKENS, ModeContext, ModeQuery, ModeSource, NoticeOnce};
use super::mode_files::{FileError, MAX_FILES_PER_MODE, MAX_MODE_TOKENS, clip, read_file};
use super::mode_index::{IDENTITY_KEY, IndexJob, identity};
use super::settings::BgMsg;
use super::store::StoreAccess;
use super::{Notice, NoticeKind};
use crate::storage::{Db, ModeOverview};

pub(crate) const MAX_MODES: usize = 50;
const KEY_SELECTED: &str = "selected_mode";

#[derive(Debug)]
pub(crate) enum ModesMsg {
    AddPaths {
        mode_id: Uuid,
        paths: Vec<PathBuf>,
    },
    FileRead {
        mode_id: Uuid,
        name: String,
        result: Result<String, FileError>,
    },
    Progress {
        file_id: Uuid,
        done: u32,
        total: u32,
    },
    Indexed {
        file_id: Uuid,
        chunks: u32,
    },
    IndexFailed {
        file_id: Uuid,
        message: String,
    },
    JobDone,
}

/// Modes and files as the database holds them at start. Read before the storage thread takes
/// the connection, so no intent can run before the state it acts on exists.
#[derive(Debug)]
pub(crate) struct SavedModes {
    overview: Vec<ModeOverview>,
    selected: Option<Uuid>,
    stored_identity: Option<serde_json::Value>,
}

impl SavedModes {
    pub fn read(db: &Db) -> Option<Self> {
        let overview = db.modes_overview().ok()?;
        let selected = db
            .get_setting(KEY_SELECTED)
            .ok()?
            .and_then(|v| v.as_str().map(str::to_string))
            .and_then(|s| s.parse().ok());
        let stored_identity = db.get_setting(IDENTITY_KEY).ok()?;
        Some(Self {
            overview,
            selected,
            stored_identity,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileState {
    Idle,
    Indexing { done: u32, total: u32 },
    Failed(String),
}

#[derive(Debug, Clone)]
struct ModeFile {
    id: Uuid,
    name: String,
    tokens: u32,
    chunks: u32,
    state: FileState,
}

#[derive(Debug, Clone)]
struct Mode {
    id: Uuid,
    name: String,
    prompt: String,
    files: Vec<ModeFile>,
}

impl Mode {
    fn total_tokens(&self) -> u32 {
        self.files.iter().fold(0, |t, f| t.saturating_add(f.tokens))
    }

    fn retrieval(&self) -> bool {
        self.total_tokens() > FULL_TEXT_MAX_TOKENS
    }

    fn fully_indexed(&self) -> bool {
        self.files.iter().all(|f| f.chunks > 0)
    }

    fn view(&self) -> ModeView {
        let retrieval = self.retrieval();
        ModeView {
            id: self.id,
            name: self.name.clone(),
            system_prompt: self.prompt.clone(),
            files: self.files.iter().map(|f| f.view(retrieval)).collect(),
            total_tokens: self.total_tokens(),
            retrieval,
        }
    }
}

impl ModeFile {
    fn view(&self, retrieval: bool) -> ModeFileView {
        let (status, done, total, error) = match (&self.state, retrieval) {
            (_, false) => (ModeFileStatus::Ready, 0, 0, None),
            (FileState::Failed(m), true) => (ModeFileStatus::Failed, 0, 0, Some(m.clone())),
            (FileState::Indexing { done, total }, true) => {
                (ModeFileStatus::Indexing, *done, *total, None)
            }
            (FileState::Idle, true) if self.chunks > 0 => (ModeFileStatus::Ready, 0, 0, None),
            (FileState::Idle, true) => (ModeFileStatus::Indexing, 0, 0, None),
        };
        ModeFileView {
            id: self.id,
            name: self.name.clone(),
            tokens: self.tokens,
            status,
            chunks_done: done,
            chunks_total: total,
            error,
        }
    }
}

struct ActiveSession {
    mode_id: Uuid,
    name: String,
    context: Arc<ModeContext>,
}

struct RunningJob {
    mode_id: Uuid,
    cancel: CancellationToken,
}

pub(crate) struct ModesState {
    access: StoreAccess,
    embedder: Arc<dyn Embedder>,
    notices: broadcast::Sender<Notice>,
    notes: Arc<NoticeOnce>,
    bg: mpsc::UnboundedSender<BgMsg>,
    modes: Vec<Mode>,
    selected: Option<Uuid>,
    /// Stored vectors come from another embedding model and are dropped before the next job.
    stale_index: bool,
    running: Option<RunningJob>,
    active: Option<ActiveSession>,
    changed: bool,
    shutdown: CancellationToken,
}

impl ModesState {
    pub fn new(
        access: StoreAccess,
        embedder: Arc<dyn Embedder>,
        notices: broadcast::Sender<Notice>,
        bg: mpsc::UnboundedSender<BgMsg>,
        saved: Option<SavedModes>,
    ) -> Self {
        let mut state = Self {
            access,
            embedder,
            notes: Arc::new(NoticeOnce::new(notices.clone())),
            notices,
            bg,
            modes: Vec::new(),
            selected: None,
            stale_index: false,
            running: None,
            active: None,
            changed: false,
            shutdown: CancellationToken::new(),
        };
        if let Some(saved) = saved {
            state.apply_saved(saved);
        }
        state
    }

    /// Starts indexing whatever the last run left unfinished.
    pub fn resume(&mut self) {
        self.maybe_index();
    }

    pub fn shutdown(&self) {
        self.shutdown.cancel();
        if let Some(job) = &self.running {
            job.cancel.cancel();
        }
    }

    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub fn views(&self) -> (Vec<ModeView>, Option<Uuid>) {
        let mut modes: Vec<&Mode> = self.modes.iter().collect();
        modes.sort_by_cached_key(|m| (m.name.to_lowercase(), m.id));
        (
            modes.into_iter().map(Mode::view).collect(),
            self.selected.filter(|id| self.find(*id).is_some()),
        )
    }

    fn find(&self, id: Uuid) -> Option<&Mode> {
        self.modes.iter().find(|m| m.id == id)
    }

    fn find_mut(&mut self, id: Uuid) -> Option<&mut Mode> {
        self.modes.iter_mut().find(|m| m.id == id)
    }

    fn notice(&self, message: impl Into<String>) {
        let _ = self.notices.send(Notice {
            kind: NoticeKind::Modes,
            message: message.into(),
        });
    }

    // ---- sessions ----

    /// Picks the mode for a new session: the one asked for, else the one selected for the next
    /// session. Starts loading its material so the first answer does not wait.
    pub fn begin_session(&mut self, requested: Option<Uuid>) -> Option<(Uuid, String)> {
        let id = requested
            .filter(|id| self.find(*id).is_some())
            .or_else(|| self.selected.filter(|id| self.find(*id).is_some()))?;
        if self.find(id).is_some_and(Mode::retrieval) {
            self.retry_failed(id);
        }
        let mode = self.find(id)?;
        let name = mode.name.clone();
        let context = self.build_context(mode);
        self.active = Some(ActiveSession {
            mode_id: id,
            name: name.clone(),
            context,
        });
        self.maybe_index();
        Some((id, name))
    }

    pub fn end_session(&mut self) {
        self.active = None;
    }

    /// The mode part of a request, filled in by the stream task.
    pub fn query(&self, text: &str) -> Option<ModeQuery> {
        self.active.as_ref().map(|a| ModeQuery {
            context: Arc::clone(&a.context),
            query: text.to_string(),
        })
    }

    fn build_context(&self, mode: &Mode) -> Arc<ModeContext> {
        let context = ModeContext::new(
            ModeSource {
                mode_id: mode.id,
                instructions: mode.prompt.clone(),
                file_count: mode.files.len(),
                total_tokens: mode.total_tokens(),
                fully_indexed: mode.fully_indexed(),
                index_failed: mode
                    .files
                    .iter()
                    .any(|f| matches!(f.state, FileState::Failed(_))),
            },
            self.access.clone(),
            Arc::clone(&self.embedder),
            Arc::clone(&self.notes),
        );
        context.preload();
        context
    }

    /// Rebuilds the running session's context after its mode changed.
    fn refresh_active(&mut self, mode_id: Uuid) {
        if self.active.as_ref().is_none_or(|a| a.mode_id != mode_id) {
            return;
        }
        let Some(mode) = self.find(mode_id) else {
            return;
        };
        let name = mode.name.clone();
        let context = self.build_context(mode);
        if let Some(a) = self.active.as_mut() {
            a.name = name;
            a.context = context;
        }
    }

    // ---- intents ----

    /// Returns whether the intent was one of the mode intents.
    pub fn on_intent(&mut self, intent: &Intent) -> bool {
        match intent {
            Intent::CreateMode {
                name,
                system_prompt,
            } => self.create(name.trim(), system_prompt),
            Intent::UpdateMode {
                mode_id,
                name,
                system_prompt,
            } => self.update(*mode_id, name.trim(), system_prompt),
            Intent::DeleteMode { mode_id } => self.delete(*mode_id),
            Intent::SelectMode { mode_id } => self.select(*mode_id),
            Intent::RemoveModeFile { mode_id, file_id } => self.remove_file(*mode_id, *file_id),
            Intent::RetryIndexing { mode_id } => {
                self.retry_failed(*mode_id);
                self.maybe_index();
            }
            // The shell opens the file panel and calls the core with the chosen paths.
            Intent::PickModeFiles { .. } => {}
            _ => return false,
        }
        true
    }

    fn create(&mut self, name: &str, prompt: &str) {
        if self.modes.len() >= MAX_MODES {
            self.notice(format!(
                "You can have up to {MAX_MODES} modes. Delete one to add another"
            ));
            return;
        }
        let id = Uuid::now_v7();
        self.modes.push(Mode {
            id,
            name: name.to_string(),
            prompt: prompt.to_string(),
            files: Vec::new(),
        });
        let (name, prompt) = (name.to_string(), prompt.to_string());
        self.access
            .write(move |db| db.create_mode_with_id(id, &name, &prompt, unix_ms()));
        self.changed = true;
    }

    fn update(&mut self, id: Uuid, name: &str, prompt: &str) {
        let Some(mode) = self.find_mut(id) else {
            return;
        };
        mode.name = name.to_string();
        mode.prompt = prompt.to_string();
        let (name, prompt) = (name.to_string(), prompt.to_string());
        self.access
            .write(move |db| db.update_mode(id, &name, &prompt, unix_ms()));
        self.refresh_active(id);
        self.changed = true;
    }

    fn delete(&mut self, id: Uuid) {
        let before = self.modes.len();
        self.modes.retain(|m| m.id != id);
        if self.modes.len() == before {
            return;
        }
        self.cancel_job_for(id);
        if self.selected == Some(id) {
            self.select(None);
        }
        self.access.write(move |db| db.delete_mode(id, unix_ms()));
        self.changed = true;
    }

    fn select(&mut self, id: Option<Uuid>) {
        if id.is_some_and(|id| self.find(id).is_none()) {
            return;
        }
        self.selected = id;
        let value = id.map_or(serde_json::Value::Null, |id| id.to_string().into());
        self.access
            .write(move |db| db.set_setting(KEY_SELECTED, &value, unix_ms()));
        self.changed = true;
    }

    fn remove_file(&mut self, mode_id: Uuid, file_id: Uuid) {
        let Some(mode) = self.find_mut(mode_id) else {
            return;
        };
        let before = mode.files.len();
        mode.files.retain(|f| f.id != file_id);
        if mode.files.len() == before {
            return;
        }
        self.cancel_job_for(mode_id);
        self.access
            .write(move |db| db.delete_mode_file(file_id, unix_ms()));
        self.refresh_active(mode_id);
        self.maybe_index();
        self.changed = true;
    }

    /// Stops the running job when it works on this mode; `on_job_done` starts the next one.
    fn cancel_job_for(&self, mode_id: Uuid) {
        if let Some(job) = self.running.as_ref().filter(|j| j.mode_id == mode_id) {
            job.cancel.cancel();
        }
    }

    fn retry_failed(&mut self, mode_id: Uuid) {
        let Some(mode) = self.find_mut(mode_id) else {
            return;
        };
        let mut any = false;
        for f in &mut mode.files {
            if matches!(f.state, FileState::Failed(_)) {
                f.state = FileState::Idle;
                any = true;
            }
        }
        if any {
            self.notes.clear();
            self.changed = true;
        }
    }

    // ---- messages ----

    pub fn on_msg(&mut self, msg: ModesMsg) {
        match msg {
            ModesMsg::AddPaths { mode_id, paths } => self.add_paths(mode_id, paths),
            ModesMsg::FileRead {
                mode_id,
                name,
                result,
            } => self.on_file_read(mode_id, name, result),
            ModesMsg::Progress {
                file_id,
                done,
                total,
            } => {
                if let Some(f) = self.file_mut(file_id) {
                    f.state = FileState::Indexing { done, total };
                    self.changed = true;
                }
            }
            ModesMsg::Indexed { file_id, chunks } => {
                if let Some(f) = self.file_mut(file_id) {
                    f.chunks = chunks;
                    f.state = FileState::Idle;
                    self.notes.clear();
                    self.changed = true;
                }
            }
            ModesMsg::IndexFailed { file_id, message } => {
                if let Some(f) = self.file_mut(file_id) {
                    f.state = FileState::Failed(message.clone());
                    self.changed = true;
                    self.notes.announce(
                        NoticeKind::Modes,
                        format!("Mode files are not indexed: {message}. Answers use the start of each file; press Retry in Settings > Modes once it is fixed"),
                    );
                }
            }
            ModesMsg::JobDone => self.on_job_done(),
        }
    }

    fn file_mut(&mut self, file_id: Uuid) -> Option<&mut ModeFile> {
        self.modes
            .iter_mut()
            .flat_map(|m| m.files.iter_mut())
            .find(|f| f.id == file_id)
    }

    fn apply_saved(&mut self, saved: SavedModes) {
        let SavedModes {
            overview,
            selected,
            stored_identity,
        } = saved;
        let any_chunks = overview.iter().flat_map(|o| &o.files).any(|f| f.chunks > 0);
        let current_identity = identity(self.embedder.as_ref());
        self.stale_index = any_chunks && stored_identity.as_ref() != Some(&current_identity);
        let stale = self.stale_index;
        self.modes = overview
            .into_iter()
            .map(|o| Mode {
                id: o.mode.id,
                name: o.mode.name,
                prompt: o.mode.system_prompt,
                files: o
                    .files
                    .into_iter()
                    .map(|f| ModeFile {
                        id: f.id,
                        name: f.file_name,
                        tokens: f.token_estimate,
                        chunks: if stale { 0 } else { f.chunks },
                        state: FileState::Idle,
                    })
                    .collect(),
            })
            .collect();
        self.selected = selected.filter(|id| self.find(*id).is_some());
        self.changed = true;
    }

    fn add_paths(&mut self, mode_id: Uuid, paths: Vec<PathBuf>) {
        if self.find(mode_id).is_none() {
            return;
        }
        let (paths, clipped) = clip(paths);
        if clipped {
            self.notice(format!(
                "Only the first {MAX_FILES_PER_MODE} files were added"
            ));
        }
        let bg = self.bg.clone();
        let cancel = self.shutdown.clone();
        tokio::spawn(async move {
            for path in paths {
                if cancel.is_cancelled() {
                    return;
                }
                let read = tokio::task::spawn_blocking(move || read_file(&path)).await;
                let (name, result) =
                    read.unwrap_or_else(|_| ("file".into(), Err(FileError::Unreadable)));
                let _ = bg.send(BgMsg::Modes(ModesMsg::FileRead {
                    mode_id,
                    name,
                    result,
                }));
            }
        });
    }

    fn on_file_read(&mut self, mode_id: Uuid, name: String, result: Result<String, FileError>) {
        let text = match result {
            Ok(text) => text,
            Err(e) => {
                self.notice(format!("{name}: {e}"));
                return;
            }
        };
        let Some(mode) = self.find_mut(mode_id) else {
            return;
        };
        let tokens = estimate_tokens(&text);
        let replaced = mode.files.iter().find(|f| f.name == name);
        let files_after = mode.files.len() - usize::from(replaced.is_some()) + 1;
        let tokens_after = mode
            .total_tokens()
            .saturating_sub(replaced.map_or(0, |f| f.tokens))
            .saturating_add(tokens);
        if files_after > MAX_FILES_PER_MODE {
            self.notice(format!(
                "{name}: a mode can have up to {MAX_FILES_PER_MODE} files"
            ));
            return;
        }
        if tokens_after > MAX_MODE_TOKENS {
            self.notice(format!(
                "{name}: the files of a mode can hold up to {} million tokens of text",
                MAX_MODE_TOKENS / 1_000_000
            ));
            return;
        }
        mode.files.retain(|f| f.name != name);
        let id = Uuid::now_v7();
        mode.files.push(ModeFile {
            id,
            name: name.clone(),
            tokens,
            chunks: 0,
            state: FileState::Idle,
        });
        mode.files.sort_by(|a, b| a.name.cmp(&b.name));
        self.cancel_job_for(mode_id);
        self.access.write(move |db| {
            db.put_mode_file(id, mode_id, &name, &text, tokens, unix_ms())
                .map(|_| ())
        });
        self.refresh_active(mode_id);
        self.maybe_index();
        self.changed = true;
    }

    // ---- indexing ----

    /// Starts embedding the next mode that needs it, unless a job is already running.
    fn maybe_index(&mut self) {
        if self.running.is_some() {
            return;
        }
        let Some(mode) = self.modes.iter().find(|m| {
            m.retrieval()
                && m.files
                    .iter()
                    .any(|f| f.chunks == 0 && f.state == FileState::Idle)
        }) else {
            return;
        };
        let mode_id = mode.id;
        let files: Vec<Uuid> = mode
            .files
            .iter()
            .filter(|f| f.chunks == 0 && f.state == FileState::Idle)
            .map(|f| f.id)
            .collect();
        let cancel = self.shutdown.child_token();
        self.running = Some(RunningJob {
            mode_id,
            cancel: cancel.clone(),
        });
        let job = IndexJob {
            files,
            purge_stale: std::mem::take(&mut self.stale_index),
            embedder: Arc::clone(&self.embedder),
            access: self.access.clone(),
            out: self.bg.clone(),
            cancel,
            now_ms: unix_ms(),
        };
        tokio::spawn(job.run());
    }

    fn on_job_done(&mut self) {
        let finished = self.running.take();
        for f in self.modes.iter_mut().flat_map(|m| m.files.iter_mut()) {
            if matches!(f.state, FileState::Indexing { .. }) {
                f.state = FileState::Idle;
            }
        }
        if let Some(job) = finished {
            self.refresh_active(job.mode_id);
        }
        if !self.shutdown.is_cancelled() {
            self.maybe_index();
        }
        self.changed = true;
    }
}
