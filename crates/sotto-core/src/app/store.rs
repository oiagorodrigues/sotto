//! SQLite calls block, so the database lives on its own thread and the event loop only queues
//! writes. The queue is unbounded on purpose: transcript revisions must never be dropped, and
//! their rate is bounded by speech.

use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

use super::{Notice, NoticeKind};
use crate::storage::{AnswerUpdate, Db, NewAnswer, SegmentRow, StorageError};

#[derive(Debug)]
enum Cmd {
    CreateSession {
        id: Uuid,
        title: String,
        mode_id: Option<Uuid>,
        now_ms: i64,
    },
    EndSession {
        id: Uuid,
        now_ms: i64,
    },
    Segment {
        session_id: Uuid,
        row: SegmentRow,
        now_ms: i64,
    },
    InsertAnswer {
        id: Uuid,
        answer: NewAnswer,
        now_ms: i64,
    },
    UpdateAnswer {
        id: Uuid,
        update: AnswerUpdate,
        now_ms: i64,
    },
    SetSetting {
        key: &'static str,
        value: serde_json::Value,
        now_ms: i64,
    },
    Purge {
        days: u32,
        now_ms: i64,
    },
    Run(RunFn),
}

/// Work for the storage thread that needs more than one statement or a result.
type DbTask = Box<dyn FnOnce(&mut Db) -> Result<(), StorageError> + Send>;

pub(crate) struct RunFn(DbTask);

impl std::fmt::Debug for RunFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunFn")
    }
}

#[derive(Debug)]
pub(crate) struct StoreHandle {
    tx: Option<mpsc::UnboundedSender<Cmd>>,
    done: Option<oneshot::Receiver<()>>,
}

impl StoreHandle {
    pub fn spawn(db: Db, notices: broadcast::Sender<Notice>) -> std::io::Result<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
        let (done_tx, done_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("sotto-storage".into())
            .spawn(move || {
                let mut db = db;
                while let Some(cmd) = rx.blocking_recv() {
                    if let Err(e) = apply(&mut db, cmd) {
                        tracing::warn!(error = %e, "storage write failed");
                        let _ = notices.send(Notice {
                            kind: NoticeKind::Storage,
                            message: format!("Could not save to the local history: {e}"),
                        });
                    }
                }
                drop(db);
                let _ = done_tx.send(());
            })?;
        Ok(Self {
            tx: Some(tx),
            done: Some(done_rx),
        })
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.tx
            && tx.send(cmd).is_err()
        {
            tracing::warn!("storage thread is gone; write lost");
        }
    }

    pub fn create_session(&self, id: Uuid, title: &str, mode_id: Option<Uuid>, now_ms: i64) {
        self.send(Cmd::CreateSession {
            id,
            title: title.to_string(),
            mode_id,
            now_ms,
        });
    }

    pub fn end_session(&self, id: Uuid, now_ms: i64) {
        self.send(Cmd::EndSession { id, now_ms });
    }

    pub fn segment(&self, session_id: Uuid, row: SegmentRow, now_ms: i64) {
        self.send(Cmd::Segment {
            session_id,
            row,
            now_ms,
        });
    }

    pub fn insert_answer(&self, id: Uuid, answer: NewAnswer, now_ms: i64) {
        self.send(Cmd::InsertAnswer { id, answer, now_ms });
    }

    pub fn update_answer(&self, id: Uuid, update: AnswerUpdate, now_ms: i64) {
        self.send(Cmd::UpdateAnswer { id, update, now_ms });
    }

    pub fn set_setting(&self, key: &'static str, value: serde_json::Value, now_ms: i64) {
        self.send(Cmd::SetSetting { key, value, now_ms });
    }

    /// Hard-deletes sessions older than the retention window.
    pub fn purge(&self, days: u32, now_ms: i64) {
        self.send(Cmd::Purge { days, now_ms });
    }

    /// A handle for code outside the event loop. It does not keep the storage thread
    /// alive, so a task that is still running at shutdown cannot delay the final flush.
    pub fn access(&self) -> StoreAccess {
        StoreAccess {
            tx: self.tx.as_ref().map(mpsc::UnboundedSender::downgrade),
        }
    }

    /// Stops accepting writes and waits until the queued ones are on disk.
    pub async fn close(&mut self) {
        self.tx = None;
        if let Some(done) = self.done.take() {
            let _ = done.await;
        }
    }
}

/// Runs closures on the storage thread, for state owned outside the event loop.
#[derive(Debug, Clone)]
pub(crate) struct StoreAccess {
    tx: Option<mpsc::WeakUnboundedSender<Cmd>>,
}

impl StoreAccess {
    /// Fire and forget; a failure becomes a storage notice.
    pub fn write(&self, f: impl FnOnce(&mut Db) -> Result<(), StorageError> + Send + 'static) {
        if let Some(tx) = self
            .tx
            .as_ref()
            .and_then(mpsc::WeakUnboundedSender::upgrade)
            && tx.send(Cmd::Run(RunFn(Box::new(f)))).is_err()
        {
            tracing::warn!("storage thread is gone; write lost");
        }
    }

    /// `None` when the storage thread has stopped.
    pub async fn query<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Db) -> Result<R, StorageError> + Send + 'static,
    ) -> Option<Result<R, StorageError>> {
        let tx = self.tx.as_ref()?.upgrade()?;
        let (reply, answer) = oneshot::channel();
        let run = RunFn(Box::new(move |db| {
            let _ = reply.send(f(db));
            Ok(())
        }));
        tx.send(Cmd::Run(run)).ok()?;
        answer.await.ok()
    }
}

fn apply(db: &mut Db, cmd: Cmd) -> Result<(), StorageError> {
    match cmd {
        Cmd::CreateSession {
            id,
            title,
            mode_id,
            now_ms,
        } => db.create_session_with_id(id, &title, mode_id, now_ms),
        Cmd::EndSession { id, now_ms } => db.end_session(id, now_ms),
        Cmd::Segment {
            session_id,
            row,
            now_ms,
        } => db.upsert_segment(session_id, &row, now_ms).map(|_| ()),
        Cmd::InsertAnswer { id, answer, now_ms } => db.insert_answer_with_id(id, &answer, now_ms),
        Cmd::UpdateAnswer { id, update, now_ms } => db.update_answer(id, &update, now_ms),
        Cmd::SetSetting { key, value, now_ms } => db.set_setting(key, &value, now_ms),
        Cmd::Run(RunFn(f)) => f(db),
        Cmd::Purge { days, now_ms } => db.purge_sessions_older_than(days, now_ms).map(|n| {
            if n > 0 {
                tracing::info!(sessions = n, "retention purge");
            }
        }),
    }
}
