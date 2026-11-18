//! Encrypted local history (ADR 0004): SQLCipher through `rusqlite`, UUIDv7 text ids, UTC
//! millisecond timestamps and `deleted_at` tombstones so the schema can sync later.

use std::fmt::Write as _;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sotto_protocol::Speaker;
use sotto_protocol::ui::{AnswerOrigin, AnswerState};
use uuid::Uuid;

use crate::providers::Usage;

const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_init.sql")];

const MS_PER_DAY: i64 = 86_400_000;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The file is not a database, or it was encrypted with another key.
    #[error("database cannot be opened with this key")]
    KeyRejected,
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid stored value: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid stored id: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("database was created by a newer version (schema {found}, supported {supported})")]
    SchemaTooNew { found: usize, supported: usize },
}

pub type Result<T, E = StorageError> = std::result::Result<T, E>;

fn enum_str<T: Serialize>(value: T) -> Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(s) => Ok(s),
        other => Ok(other.to_string()),
    }
}

fn from_enum_str<T: serde::de::DeserializeOwned>(s: String) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(s))?)
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn new_id() -> String {
    Uuid::now_v7().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: Uuid,
    pub title: String,
    pub mode_id: Option<Uuid>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRow {
    pub speaker: Speaker,
    pub segment_id: u64,
    pub revision: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub is_final: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAnswer {
    pub session_id: Uuid,
    pub origin: AnswerOrigin,
    pub question: String,
    pub model: String,
    pub state: AnswerState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnswerUpdate {
    pub text: String,
    pub state: AnswerState,
    pub latency_ms: Option<u32>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnswerRow {
    pub id: Uuid,
    pub session_id: Uuid,
    pub origin: AnswerOrigin,
    pub question: String,
    pub text: String,
    pub model: String,
    pub latency_ms: Option<u32>,
    pub state: AnswerState,
    pub usage: Option<Usage>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeRow {
    pub id: Uuid,
    pub name: String,
    pub system_prompt: String,
}

/// A mode as sync sees it: live or tombstoned, with its last change time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeSyncRow {
    pub id: Uuid,
    pub name: String,
    pub system_prompt: String,
    pub updated_ms: i64,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeFileRow {
    pub id: Uuid,
    pub mode_id: Uuid,
    pub file_name: String,
    pub text: String,
    pub token_estimate: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewChunk {
    pub ordinal: u32,
    pub text: String,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredChunk {
    pub chunk_id: Uuid,
    pub mode_file_id: Uuid,
    pub file_name: String,
    pub ordinal: u32,
    pub text: String,
    pub score: f32,
}

/// A mode file without its text, with how many chunks are stored for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeFileMeta {
    pub id: Uuid,
    pub file_name: String,
    pub token_estimate: u32,
    pub chunks: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeOverview {
    pub mode: ModeRow,
    pub files: Vec<ModeFileMeta>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredChunk {
    pub file_name: String,
    pub ordinal: u32,
    pub text: String,
    pub embedding: Vec<f32>,
}

pub fn encode_embedding(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    let (chunks, _) = bytes.as_chunks::<4>();
    chunks.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

pub use sotto_rag::cosine;

/// Handle to the encrypted database. Not `Sync`; wrap it in a mutex or own it on one task.
#[derive(Debug)]
pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path, key: [u8; 32]) -> Result<Self> {
        let conn = Connection::open(path)?;
        let mut hex = String::with_capacity(64);
        for byte in key {
            // Writing to a String cannot fail.
            let _ = write!(hex, "{byte:02x}");
        }
        // Raw-key syntax skips SQLCipher's passphrase KDF; the key is already 256 random bits.
        conn.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))?;
        if let Err(e) = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        }) {
            return Err(match e.sqlite_error_code() {
                Some(rusqlite::ErrorCode::NotADatabase) => StorageError::KeyRejected,
                _ => e.into(),
            });
        }
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        let mut db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&mut self) -> Result<()> {
        let current: usize = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .map(|v| usize::try_from(v).unwrap_or(0))?;
        if current > MIGRATIONS.len() {
            return Err(StorageError::SchemaTooNew {
                found: current,
                supported: MIGRATIONS.len(),
            });
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {};", i + 1))?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<usize> {
        let v: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        Ok(usize::try_from(v).unwrap_or(0))
    }

    pub fn create_session(&self, title: &str, mode_id: Option<Uuid>, now_ms: i64) -> Result<Uuid> {
        let id = Uuid::now_v7();
        self.create_session_with_id(id, title, mode_id, now_ms)?;
        Ok(id)
    }

    /// Same as [`create_session`](Self::create_session) with an id the caller already showed
    /// in the UI.
    pub fn create_session_with_id(
        &self,
        id: Uuid,
        title: &str,
        mode_id: Option<Uuid>,
        now_ms: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, title, mode_id, started_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4, ?4)",
            params![
                id.to_string(),
                title,
                mode_id.map(|m| m.to_string()),
                now_ms
            ],
        )?;
        Ok(())
    }

    pub fn end_session(&self, id: Uuid, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?2, updated_at = ?2 WHERE id = ?1 AND ended_at IS NULL",
            params![id.to_string(), now_ms],
        )?;
        Ok(())
    }

    /// Soft delete: the row stays as a tombstone until the retention purge removes it.
    pub fn delete_session(&self, id: Uuid, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![id.to_string(), now_ms],
        )?;
        Ok(())
    }

    /// Newest first, tombstones excluded.
    pub fn list_sessions(&self, limit: u32) -> Result<Vec<SessionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, mode_id, started_at, ended_at FROM sessions
             WHERE deleted_at IS NULL ORDER BY started_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, title, mode_id, started_at, ended_at) = row?;
            out.push(SessionRow {
                id: id.parse()?,
                title,
                mode_id: mode_id.map(|m| m.parse()).transpose()?,
                started_at,
                ended_at,
            });
        }
        Ok(out)
    }

    /// Stores a segment revision only if it is newer than the stored one. An empty text revokes
    /// the segment (tombstone); a later non-empty revision brings it back. Returns whether the
    /// row changed.
    pub fn upsert_segment(&self, session_id: Uuid, seg: &SegmentRow, now_ms: i64) -> Result<bool> {
        let deleted_at: Option<i64> = seg.text.is_empty().then_some(now_ms);
        let changed = self.conn.execute(
            "INSERT INTO transcript_segments
               (id, session_id, speaker, segment_id, revision, start_ms, end_ms, text, is_final,
                created_at, updated_at, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11)
             ON CONFLICT (session_id, speaker, segment_id) DO UPDATE SET
               revision = excluded.revision, start_ms = excluded.start_ms,
               end_ms = excluded.end_ms, text = excluded.text, is_final = excluded.is_final,
               updated_at = excluded.updated_at, deleted_at = excluded.deleted_at
             WHERE excluded.revision > transcript_segments.revision",
            params![
                new_id(),
                session_id.to_string(),
                enum_str(seg.speaker)?,
                to_i64(seg.segment_id),
                seg.revision,
                to_i64(seg.start_ms),
                to_i64(seg.end_ms),
                seg.text,
                seg.is_final,
                now_ms,
                deleted_at,
            ],
        )?;
        Ok(changed > 0)
    }

    /// Live (non-revoked) segments in time order.
    pub fn list_segments(&self, session_id: Uuid) -> Result<Vec<SegmentRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT speaker, segment_id, revision, start_ms, end_ms, text, is_final
             FROM transcript_segments WHERE session_id = ?1 AND deleted_at IS NULL
             ORDER BY start_ms, segment_id",
        )?;
        let rows = stmt.query_map([session_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, bool>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (speaker, segment_id, revision, start, end, text, is_final) = row?;
            out.push(SegmentRow {
                speaker: from_enum_str(speaker)?,
                segment_id: u64::try_from(segment_id).unwrap_or(0),
                revision,
                start_ms: u64::try_from(start).unwrap_or(0),
                end_ms: u64::try_from(end).unwrap_or(0),
                text,
                is_final,
            });
        }
        Ok(out)
    }

    pub fn insert_answer(&self, a: &NewAnswer, now_ms: i64) -> Result<Uuid> {
        let id = Uuid::now_v7();
        self.insert_answer_with_id(id, a, now_ms)?;
        Ok(id)
    }

    /// Same as [`insert_answer`](Self::insert_answer) with an id the caller already showed in
    /// the UI.
    pub fn insert_answer_with_id(&self, id: Uuid, a: &NewAnswer, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO answers (id, session_id, origin, question, text, model, state,
                                  created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7, ?7)",
            params![
                id.to_string(),
                a.session_id.to_string(),
                enum_str(a.origin)?,
                a.question,
                a.model,
                enum_str(a.state)?,
                now_ms,
            ],
        )?;
        Ok(())
    }

    pub fn update_answer(&self, id: Uuid, u: &AnswerUpdate, now_ms: i64) -> Result<()> {
        let usage = u.usage.as_ref().map(serde_json::to_string).transpose()?;
        self.conn.execute(
            "UPDATE answers SET text = ?2, state = ?3, latency_ms = ?4, usage = ?5, updated_at = ?6
             WHERE id = ?1",
            params![
                id.to_string(),
                u.text,
                enum_str(u.state)?,
                u.latency_ms,
                usage,
                now_ms
            ],
        )?;
        Ok(())
    }

    pub fn list_answers(&self, session_id: Uuid) -> Result<Vec<AnswerRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, origin, question, text, model, latency_ms, state, usage, created_at
             FROM answers WHERE session_id = ?1 AND deleted_at IS NULL
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([session_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<u32>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, origin, question, text, model, latency_ms, state, usage, created_at) = row?;
            out.push(AnswerRow {
                id: id.parse()?,
                session_id,
                origin: from_enum_str(origin)?,
                question,
                text,
                model,
                latency_ms,
                state: from_enum_str(state)?,
                usage: usage.map(|u| serde_json::from_str(&u)).transpose()?,
                created_at,
            });
        }
        Ok(out)
    }

    /// Hard-deletes sessions (and their segments and answers) that started more than `days`
    /// days before `now_ms`, tombstoned or not. Returns the number of sessions removed.
    pub fn purge_sessions_older_than(&self, days: u32, now_ms: i64) -> Result<usize> {
        let cutoff = now_ms.saturating_sub(i64::from(days).saturating_mul(MS_PER_DAY));
        Ok(self
            .conn
            .execute("DELETE FROM sessions WHERE started_at < ?1", [cutoff])?)
    }

    pub fn create_mode(&self, name: &str, system_prompt: &str, now_ms: i64) -> Result<Uuid> {
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO modes (id, name, system_prompt, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id.to_string(), name, system_prompt, now_ms],
        )?;
        Ok(id)
    }

    pub fn create_mode_with_id(
        &self,
        id: Uuid,
        name: &str,
        system_prompt: &str,
        now_ms: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO modes (id, name, system_prompt, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id.to_string(), name, system_prompt, now_ms],
        )?;
        Ok(())
    }

    pub fn update_mode(
        &self,
        id: Uuid,
        name: &str,
        system_prompt: &str,
        now_ms: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE modes SET name = ?2, system_prompt = ?3, updated_at = ?4
             WHERE id = ?1 AND deleted_at IS NULL",
            params![id.to_string(), name, system_prompt, now_ms],
        )?;
        Ok(())
    }

    /// Every mode, tombstones included, for preference sync.
    pub fn modes_for_sync(&self) -> Result<Vec<ModeSyncRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, system_prompt, updated_at, deleted_at IS NOT NULL FROM modes ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, bool>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, system_prompt, updated_ms, deleted) = row?;
            out.push(ModeSyncRow {
                id: id.parse()?,
                name,
                system_prompt,
                updated_ms,
                deleted,
            });
        }
        Ok(out)
    }

    /// Writes a mode that came from another device, with that device's edit time. A tombstone
    /// with the same id is lifted.
    pub fn upsert_synced_mode(
        &self,
        id: Uuid,
        name: &str,
        system_prompt: &str,
        updated_ms: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO modes (id, name, system_prompt, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(id) DO UPDATE SET name = ?2, system_prompt = ?3, updated_at = ?4,
                                           deleted_at = NULL",
            params![id.to_string(), name, system_prompt, updated_ms],
        )?;
        Ok(())
    }

    /// Tombstones the mode and its files. A tombstone keeps the id and the name so a later sync
    /// can propagate the deletion; the prompt, the file text and the embeddings are erased.
    pub fn delete_mode(&mut self, id: Uuid, now_ms: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        let id = id.to_string();
        tx.execute(
            "DELETE FROM chunks WHERE mode_file_id IN (SELECT id FROM mode_files WHERE mode_id = ?1)",
            [&id],
        )?;
        tx.execute(
            "UPDATE mode_files SET text = '', deleted_at = ?2, updated_at = ?2
             WHERE mode_id = ?1 AND deleted_at IS NULL",
            params![id, now_ms],
        )?;
        tx.execute(
            "UPDATE modes SET system_prompt = '', deleted_at = ?2, updated_at = ?2
             WHERE id = ?1 AND deleted_at IS NULL",
            params![id, now_ms],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Stores a file, tombstoning any live file of the same mode with the same name first.
    /// Returns the ids it replaced.
    pub fn put_mode_file(
        &mut self,
        id: Uuid,
        mode_id: Uuid,
        file_name: &str,
        text: &str,
        token_estimate: u32,
        now_ms: i64,
    ) -> Result<Vec<Uuid>> {
        let tx = self.conn.transaction()?;
        let mut replaced = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT id FROM mode_files WHERE mode_id = ?1 AND file_name = ?2 AND deleted_at IS NULL",
            )?;
            for row in stmt.query_map(params![mode_id.to_string(), file_name], |r| {
                r.get::<_, String>(0)
            })? {
                replaced.push(row?);
            }
        }
        for old in &replaced {
            tx.execute("DELETE FROM chunks WHERE mode_file_id = ?1", [old])?;
            tx.execute(
                "UPDATE mode_files SET text = '', deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
                params![old, now_ms],
            )?;
        }
        tx.execute(
            "INSERT INTO mode_files (id, mode_id, file_name, text, token_estimate, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![id.to_string(), mode_id.to_string(), file_name, text, token_estimate, now_ms],
        )?;
        tx.commit()?;
        replaced
            .into_iter()
            .map(|s| s.parse().map_err(StorageError::from))
            .collect()
    }

    /// Tombstones one file; its text and embeddings are erased.
    pub fn delete_mode_file(&mut self, id: Uuid, now_ms: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        let id = id.to_string();
        tx.execute("DELETE FROM chunks WHERE mode_file_id = ?1", [&id])?;
        tx.execute(
            "UPDATE mode_files SET text = '', deleted_at = ?2, updated_at = ?2
             WHERE id = ?1 AND deleted_at IS NULL",
            params![id, now_ms],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Live modes with their live files (no text) and stored chunk counts.
    pub fn modes_overview(&self) -> Result<Vec<ModeOverview>> {
        let mut out: Vec<ModeOverview> = self
            .list_modes()?
            .into_iter()
            .map(|mode| ModeOverview {
                mode,
                files: Vec::new(),
            })
            .collect();
        let mut stmt = self.conn.prepare(
            "SELECT f.id, f.mode_id, f.file_name, f.token_estimate,
                    (SELECT count(*) FROM chunks c WHERE c.mode_file_id = f.id AND c.deleted_at IS NULL)
             FROM mode_files f WHERE f.deleted_at IS NULL ORDER BY f.file_name, f.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
                r.get::<_, u32>(4)?,
            ))
        })?;
        for row in rows {
            let (id, mode_id, file_name, token_estimate, chunks) = row?;
            let mode_id: Uuid = mode_id.parse()?;
            if let Some(o) = out.iter_mut().find(|o| o.mode.id == mode_id) {
                o.files.push(ModeFileMeta {
                    id: id.parse()?,
                    file_name,
                    token_estimate,
                    chunks,
                });
            }
        }
        Ok(out)
    }

    /// The first `max_chars` characters of every live file of the mode, by file name.
    pub fn mode_file_heads(&self, mode_id: Uuid, max_chars: u32) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT file_name, substr(text, 1, ?2) FROM mode_files
             WHERE mode_id = ?1 AND deleted_at IS NULL ORDER BY file_name, id",
        )?;
        let rows = stmt.query_map(params![mode_id.to_string(), max_chars], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Name and text of one live file.
    pub fn mode_file_text(&self, id: Uuid) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT file_name, text FROM mode_files WHERE id = ?1 AND deleted_at IS NULL",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Every stored chunk of the live files of a mode, in file and chunk order.
    pub fn mode_chunks(&self, mode_id: Uuid) -> Result<Vec<StoredChunk>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.file_name, c.ordinal, c.text, c.embedding
             FROM chunks c JOIN mode_files f ON f.id = c.mode_file_id
             WHERE f.mode_id = ?1 AND c.deleted_at IS NULL AND f.deleted_at IS NULL
             ORDER BY f.file_name, f.id, c.ordinal",
        )?;
        let rows = stmt.query_map([mode_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (file_name, ordinal, text, blob) = row?;
            out.push(StoredChunk {
                file_name,
                ordinal,
                text,
                embedding: decode_embedding(&blob),
            });
        }
        Ok(out)
    }

    /// Like [`replace_chunks`](Self::replace_chunks), but only while the file is live, so an
    /// indexing task that outlives a delete cannot bring text back. Returns whether it stored.
    pub fn replace_chunks_if_live(
        &mut self,
        mode_file_id: Uuid,
        chunks: &[NewChunk],
        now_ms: i64,
    ) -> Result<bool> {
        let live: bool = self.conn.query_row(
            "SELECT count(*) > 0 FROM mode_files f JOIN modes m ON m.id = f.mode_id
             WHERE f.id = ?1 AND f.deleted_at IS NULL AND m.deleted_at IS NULL",
            [mode_file_id.to_string()],
            |r| r.get(0),
        )?;
        if live {
            self.replace_chunks(mode_file_id, chunks, now_ms)?;
        }
        Ok(live)
    }

    /// Drops every stored embedding, for when the embedding model changed.
    pub fn clear_chunks(&self) -> Result<()> {
        self.conn.execute("DELETE FROM chunks", [])?;
        Ok(())
    }

    pub fn list_modes(&self) -> Result<Vec<ModeRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, system_prompt FROM modes WHERE deleted_at IS NULL ORDER BY name, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, system_prompt) = row?;
            out.push(ModeRow {
                id: id.parse()?,
                name,
                system_prompt,
            });
        }
        Ok(out)
    }

    pub fn add_mode_file(
        &self,
        mode_id: Uuid,
        file_name: &str,
        text: &str,
        token_estimate: u32,
        now_ms: i64,
    ) -> Result<Uuid> {
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO mode_files (id, mode_id, file_name, text, token_estimate, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![id.to_string(), mode_id.to_string(), file_name, text, token_estimate, now_ms],
        )?;
        Ok(id)
    }

    pub fn list_mode_files(&self, mode_id: Uuid) -> Result<Vec<ModeFileRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_name, text, token_estimate FROM mode_files
             WHERE mode_id = ?1 AND deleted_at IS NULL ORDER BY file_name, id",
        )?;
        let rows = stmt.query_map([mode_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, file_name, text, token_estimate) = row?;
            out.push(ModeFileRow {
                id: id.parse()?,
                mode_id,
                file_name,
                text,
                token_estimate,
            });
        }
        Ok(out)
    }

    /// Replaces all chunks of a file in one transaction.
    pub fn replace_chunks(
        &mut self,
        mode_file_id: Uuid,
        chunks: &[NewChunk],
        now_ms: i64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM chunks WHERE mode_file_id = ?1",
            [mode_file_id.to_string()],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO chunks (id, mode_file_id, ordinal, text, embedding, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            )?;
            for c in chunks {
                stmt.execute(params![
                    new_id(),
                    mode_file_id.to_string(),
                    c.ordinal,
                    c.text,
                    encode_embedding(&c.embedding),
                    now_ms,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Brute-force cosine ranking over every live chunk of the mode (ADR 0004). Chunks whose
    /// embedding length differs from the query are skipped.
    pub fn top_k_chunks(&self, mode_id: Uuid, query: &[f32], k: usize) -> Result<Vec<ScoredChunk>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.mode_file_id, f.file_name, c.ordinal, c.text, c.embedding
             FROM chunks c JOIN mode_files f ON f.id = c.mode_file_id
             WHERE f.mode_id = ?1 AND c.deleted_at IS NULL AND f.deleted_at IS NULL",
        )?;
        let rows = stmt.query_map([mode_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Vec<u8>>(5)?,
            ))
        })?;
        let mut scored = Vec::new();
        for row in rows {
            let (id, file_id, file_name, ordinal, text, blob) = row?;
            let Some(score) = cosine(query, &decode_embedding(&blob)) else {
                continue;
            };
            scored.push(ScoredChunk {
                chunk_id: id.parse()?,
                mode_file_id: file_id.parse()?,
                file_name,
                ordinal,
                text,
                score,
            });
        }
        scored.sort_by(|a, b| b.score.total_cmp(&a.score));
        scored.truncate(k);
        Ok(scored)
    }

    pub fn set_setting(&self, key: &str, value: &serde_json::Value, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, serde_json::to_string(value)?, now_ms],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    fn open_temp() -> (tempfile::TempDir, std::path::PathBuf, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sotto.db");
        let db = Db::open(&path, KEY).unwrap();
        (dir, path, db)
    }

    fn seg(id: u64, rev: u32, text: &str) -> SegmentRow {
        SegmentRow {
            speaker: Speaker::Them,
            segment_id: id,
            revision: rev,
            start_ms: id * 1000,
            end_ms: id * 1000 + 900,
            text: text.into(),
            is_final: false,
        }
    }

    #[test]
    fn wrong_key_and_no_key_fail() {
        let (_dir, path, db) = open_temp();
        db.create_session("secret meeting", None, 1).unwrap();
        drop(db);

        assert!(matches!(
            Db::open(&path, [8; 32]),
            Err(StorageError::KeyRejected)
        ));

        // Plain SQLite access without a key sees no readable database.
        let plain = Connection::open(&path).unwrap();
        let r = plain.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        });
        assert!(r.is_err());

        // The plaintext never appears in the file.
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(14).any(|w| w == b"secret meeting"));
        assert!(!bytes.starts_with(b"SQLite format 3"));

        assert!(Db::open(&path, KEY).is_ok());
    }

    #[test]
    fn migrations_are_idempotent() {
        let (_dir, path, db) = open_temp();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len());
        db.create_session("a", None, 1).unwrap();
        drop(db);
        let db = Db::open(&path, KEY).unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len());
        assert_eq!(db.list_sessions(10).unwrap().len(), 1);
    }

    #[test]
    fn session_lifecycle_and_listing() {
        let (_dir, _p, db) = open_temp();
        let a = db.create_session("first", None, 100).unwrap();
        let b = db.create_session("second", None, 200).unwrap();
        db.end_session(a, 150).unwrap();
        let list = db.list_sessions(10).unwrap();
        assert_eq!(list.iter().map(|s| s.id).collect::<Vec<_>>(), vec![b, a]);
        assert_eq!(list[1].ended_at, Some(150));
        db.delete_session(b, 300).unwrap();
        assert_eq!(db.list_sessions(10).unwrap().len(), 1);
    }

    #[test]
    fn segment_keeps_only_latest_revision() {
        let (_dir, _p, db) = open_temp();
        let s = db.create_session("s", None, 1).unwrap();
        assert!(db.upsert_segment(s, &seg(1, 1, "como voce"), 10).unwrap());
        assert!(
            db.upsert_segment(s, &seg(1, 3, "como voce faria"), 11)
                .unwrap()
        );
        assert!(!db.upsert_segment(s, &seg(1, 2, "stale"), 12).unwrap());
        let rows = db.list_segments(s).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "como voce faria");
        assert_eq!(rows[0].revision, 3);

        assert!(db.upsert_segment(s, &seg(1, 4, ""), 13).unwrap());
        assert!(db.list_segments(s).unwrap().is_empty());
        assert!(db.upsert_segment(s, &seg(1, 5, "back"), 14).unwrap());
        assert_eq!(db.list_segments(s).unwrap().len(), 1);
    }

    #[test]
    fn answer_insert_update_list() {
        let (_dir, _p, db) = open_temp();
        let s = db.create_session("s", None, 1).unwrap();
        let id = db
            .insert_answer(
                &NewAnswer {
                    session_id: s,
                    origin: AnswerOrigin::Auto,
                    question: "why?".into(),
                    model: "claude-haiku-5-5".into(),
                    state: AnswerState::Preparing,
                },
                5,
            )
            .unwrap();
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 20,
            ..Usage::default()
        };
        db.update_answer(
            id,
            &AnswerUpdate {
                text: "because".into(),
                state: AnswerState::Done,
                latency_ms: Some(640),
                usage: Some(usage),
            },
            6,
        )
        .unwrap();
        let rows = db.list_answers(s).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "because");
        assert_eq!(rows[0].state, AnswerState::Done);
        assert_eq!(rows[0].origin, AnswerOrigin::Auto);
        assert_eq!(rows[0].latency_ms, Some(640));
        assert_eq!(rows[0].usage, Some(usage));
    }

    #[test]
    fn purge_removes_old_sessions_with_children() {
        let (_dir, _p, db) = open_temp();
        let day = MS_PER_DAY;
        let now = 100 * day;
        let old = db.create_session("old", None, now - 40 * day).unwrap();
        let new = db.create_session("new", None, now - 5 * day).unwrap();
        db.upsert_segment(old, &seg(1, 1, "x"), 1).unwrap();
        db.upsert_segment(new, &seg(1, 1, "y"), 1).unwrap();
        assert_eq!(db.purge_sessions_older_than(30, now).unwrap(), 1);
        let ids: Vec<_> = db
            .list_sessions(10)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec![new]);
        assert!(db.list_segments(old).unwrap().is_empty());
        assert_eq!(db.list_segments(new).unwrap().len(), 1);
    }

    #[test]
    fn cosine_ranking_orders_chunks() {
        let (_dir, _p, mut db) = open_temp();
        let mode = db.create_mode("interview", "be brief", 1).unwrap();
        let other = db.create_mode("other", "x", 1).unwrap();
        let file = db.add_mode_file(mode, "cv.md", "text", 10, 1).unwrap();
        let other_file = db.add_mode_file(other, "o.md", "text", 10, 1).unwrap();
        let chunk = |ordinal, text: &str, e: Vec<f32>| NewChunk {
            ordinal,
            text: text.into(),
            embedding: e,
        };
        db.replace_chunks(
            file,
            &[
                chunk(0, "north", vec![0.0, 1.0, 0.0]),
                chunk(1, "east", vec![1.0, 0.0, 0.0]),
                chunk(2, "north-east", vec![1.0, 1.0, 0.0]),
                chunk(3, "wrong dims", vec![1.0, 0.0]),
            ],
            2,
        )
        .unwrap();
        db.replace_chunks(other_file, &[chunk(0, "leak", vec![1.0, 0.0, 0.0])], 2)
            .unwrap();

        let top = db.top_k_chunks(mode, &[1.0, 0.1, 0.0], 2).unwrap();
        assert_eq!(
            top.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["east", "north-east"]
        );
        assert!(top[0].score > top[1].score);
        assert_eq!(top[0].file_name, "cv.md");
        assert_eq!(
            db.top_k_chunks(mode, &[1.0, 0.0, 0.0], 10).unwrap().len(),
            3
        );
    }

    #[test]
    fn embedding_round_trip_and_cosine_edges() {
        let v = vec![0.5f32, -1.25, 3.0];
        assert_eq!(decode_embedding(&encode_embedding(&v)), v);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), None);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), None);
        assert!((cosine(&[2.0, 0.0], &[5.0, 0.0]).unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn settings_round_trip() {
        let (_dir, _p, db) = open_temp();
        assert_eq!(db.get_setting("retention_days").unwrap(), None);
        db.set_setting("retention_days", &serde_json::json!(30), 1)
            .unwrap();
        db.set_setting("retention_days", &serde_json::json!(60), 2)
            .unwrap();
        assert_eq!(
            db.get_setting("retention_days").unwrap(),
            Some(serde_json::json!(60))
        );
    }

    fn nc(ordinal: u32, text: &str) -> NewChunk {
        NewChunk {
            ordinal,
            text: text.into(),
            embedding: vec![1.0, 0.0],
        }
    }

    fn raw_count(db: &Db, sql: &str) -> i64 {
        db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn mode_crud_and_tombstones_erase_content() {
        let (_dir, _p, mut db) = open_temp();
        let id = Uuid::now_v7();
        db.create_mode_with_id(id, "Sales", "secret prompt", 1)
            .unwrap();
        db.update_mode(id, "Sales EU", "new secret prompt", 2)
            .unwrap();
        let modes = db.list_modes().unwrap();
        assert_eq!(modes.len(), 1);
        assert_eq!(modes[0].name, "Sales EU");
        assert_eq!(modes[0].system_prompt, "new secret prompt");

        let file = Uuid::now_v7();
        db.put_mode_file(file, id, "a.md", "private file text", 5, 3)
            .unwrap();
        db.replace_chunks(file, &[nc(0, "private chunk")], 3)
            .unwrap();
        let session = db.create_session("m", Some(id), 4).unwrap();

        db.delete_mode(id, 5).unwrap();
        assert!(db.list_modes().unwrap().is_empty());
        assert!(db.list_mode_files(id).unwrap().is_empty());
        assert!(db.modes_overview().unwrap().is_empty());
        assert!(db.mode_chunks(id).unwrap().is_empty());
        // The tombstones keep ids and names but no content.
        assert_eq!(
            raw_count(
                &db,
                "SELECT count(*) FROM modes WHERE deleted_at IS NOT NULL AND system_prompt = ''"
            ),
            1
        );
        assert_eq!(
            raw_count(
                &db,
                "SELECT count(*) FROM mode_files WHERE deleted_at IS NOT NULL AND text = ''"
            ),
            1
        );
        assert_eq!(raw_count(&db, "SELECT count(*) FROM chunks"), 0);
        // History that points at the mode survives.
        assert_eq!(db.list_sessions(5).unwrap()[0].id, session);
        // Updating a deleted mode does nothing.
        db.update_mode(id, "back", "again", 6).unwrap();
        assert!(db.list_modes().unwrap().is_empty());
    }

    #[test]
    fn a_file_with_the_same_name_replaces_the_old_one() {
        let (_dir, _p, mut db) = open_temp();
        let mode = db.create_mode("m", "", 1).unwrap();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        assert!(
            db.put_mode_file(a, mode, "x.txt", "one", 1, 2)
                .unwrap()
                .is_empty()
        );
        db.replace_chunks(a, &[nc(0, "old")], 2).unwrap();
        assert_eq!(
            db.put_mode_file(b, mode, "x.txt", "two", 1, 3).unwrap(),
            vec![a]
        );
        let files = db.list_mode_files(mode).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].id, b);
        assert_eq!(files[0].text, "two");
        assert_eq!(raw_count(&db, "SELECT count(*) FROM chunks"), 0);
    }

    #[test]
    fn overview_counts_chunks_and_skips_deleted_files() {
        let (_dir, _p, mut db) = open_temp();
        let mode = db.create_mode("m", "p", 1).unwrap();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        db.put_mode_file(a, mode, "a.md", "aaa", 3, 1).unwrap();
        db.put_mode_file(b, mode, "b.md", "bbb", 4, 1).unwrap();
        db.replace_chunks(a, &[nc(0, "x"), nc(1, "y")], 1).unwrap();
        db.delete_mode_file(b, 2).unwrap();
        let overview = db.modes_overview().unwrap();
        assert_eq!(overview.len(), 1);
        assert_eq!(overview[0].files.len(), 1);
        assert_eq!(overview[0].files[0].file_name, "a.md");
        assert_eq!(overview[0].files[0].chunks, 2);
        assert_eq!(overview[0].files[0].token_estimate, 3);
    }

    #[test]
    fn heads_are_cut_in_the_database() {
        let (_dir, _p, mut db) = open_temp();
        let mode = db.create_mode("m", "", 1).unwrap();
        db.put_mode_file(Uuid::now_v7(), mode, "a.md", "abcdefghij", 3, 1)
            .unwrap();
        db.put_mode_file(Uuid::now_v7(), mode, "b.md", "çãoçãoção", 3, 1)
            .unwrap();
        assert_eq!(
            db.mode_file_heads(mode, 4).unwrap(),
            vec![
                ("a.md".to_string(), "abcd".to_string()),
                ("b.md".to_string(), "çãoç".to_string())
            ]
        );
    }

    #[test]
    fn chunks_of_a_deleted_file_are_not_stored_late() {
        let (_dir, _p, mut db) = open_temp();
        let mode = db.create_mode("m", "", 1).unwrap();
        let file = Uuid::now_v7();
        db.put_mode_file(file, mode, "a.md", "text", 1, 1).unwrap();
        assert!(
            db.replace_chunks_if_live(file, &[nc(0, "kept")], 2)
                .unwrap()
        );
        assert_eq!(db.mode_chunks(mode).unwrap().len(), 1);
        db.delete_mode_file(file, 3).unwrap();
        assert!(
            !db.replace_chunks_if_live(file, &[nc(0, "late")], 4)
                .unwrap()
        );
        assert_eq!(raw_count(&db, "SELECT count(*) FROM chunks"), 0);
        // A deleted mode also stops late writes for its files.
        let other = Uuid::now_v7();
        db.put_mode_file(other, mode, "b.md", "text", 1, 5).unwrap();
        db.delete_mode(mode, 6).unwrap();
        assert!(
            !db.replace_chunks_if_live(other, &[nc(0, "late")], 7)
                .unwrap()
        );
    }

    #[test]
    fn stored_chunks_come_back_in_file_and_chunk_order_with_vectors() {
        let (_dir, _p, mut db) = open_temp();
        let mode = db.create_mode("m", "", 1).unwrap();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        db.put_mode_file(b, mode, "b.md", "x", 1, 1).unwrap();
        db.put_mode_file(a, mode, "a.md", "x", 1, 1).unwrap();
        db.replace_chunks(b, &[nc(0, "b0")], 1).unwrap();
        db.replace_chunks(a, &[nc(1, "a1"), nc(0, "a0")], 1)
            .unwrap();
        let got: Vec<_> = db
            .mode_chunks(mode)
            .unwrap()
            .into_iter()
            .map(|c| (c.file_name, c.ordinal, c.embedding))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a.md".to_string(), 0, vec![1.0, 0.0]),
                ("a.md".to_string(), 1, vec![1.0, 0.0]),
                ("b.md".to_string(), 0, vec![1.0, 0.0]),
            ]
        );
        db.clear_chunks().unwrap();
        assert!(db.mode_chunks(mode).unwrap().is_empty());
    }
}
