//! Modes, mode files and retrieval through the whole core, with a fake embedder.

use std::sync::atomic::Ordering;

use sotto_protocol::ui::{ModeFileStatus, ModeView};
use sotto_rag::EmbedError;
use sotto_rag::embed::fake::FakeEmbedder;
use uuid::Uuid;

use super::*;
use crate::storage::NewChunk;

const SMALL: Uuid = Uuid::from_u128(0x1001);
const BIG: Uuid = Uuid::from_u128(0x1002);
const HEAD: &str = "HEAD-MARKER";
const TAIL: &str = "TAIL-MARKER";
const PASSAGES: [&str; 3] = [
    "The refund window is 30 days after purchase.",
    "Parking is free for staff on level two.",
    "Support hours are nine to six on weekdays.",
];

fn identity_of(fake: &FakeEmbedder) -> serde_json::Value {
    let p = sotto_rag::Embedder::pinned(fake);
    serde_json::json!({ "name": p.name, "digest": p.digest })
}

fn big_text() -> String {
    format!("{HEAD} {} {TAIL}", "lorem ipsum dolor ".repeat(20_000))
}

fn seed_modes(db: &mut Db) {
    db.create_mode_with_id(SMALL, "Support", "Answer like a support agent.", 1)
        .unwrap();
    db.add_mode_file(SMALL, "faq.md", "Refunds are allowed within 30 days.", 9, 1)
        .unwrap();
    db.create_mode_with_id(BIG, "Handbook", "Use the handbook.", 1)
        .unwrap();
    let file = db
        .add_mode_file(BIG, "handbook.md", &big_text(), 40_000, 1)
        .unwrap();
    let chunks: Vec<NewChunk> = PASSAGES
        .iter()
        .enumerate()
        .map(|(i, text)| NewChunk {
            ordinal: u32::try_from(i).unwrap(),
            text: (*text).to_string(),
            embedding: FakeEmbedder::vector(text),
        })
        .collect();
    db.replace_chunks(file, &chunks, 1).unwrap();
    db.set_setting("rag_embedding", &identity_of(&FakeEmbedder::default()), 1)
        .unwrap();
}

fn seed_stale_index(db: &mut Db) {
    seed_modes(db);
    db.set_setting(
        "rag_embedding",
        &serde_json::json!({"name": "old-model", "digest": "x"}),
        2,
    )
    .unwrap();
}

fn with_modes(fake: &Arc<FakeEmbedder>) -> Opts {
    Opts {
        seed_mut: Some(seed_modes),
        embedder: Some(Arc::clone(fake) as Arc<dyn sotto_rag::Embedder>),
        ..Opts::default()
    }
}

fn mode(s: &AppSnapshot, id: Uuid) -> &ModeView {
    s.modes.iter().find(|m| m.id == id).unwrap()
}

async fn start_with(h: &mut Harness, mode_id: Option<Uuid>) -> FakeSidecar {
    h.app.dispatch(Intent::StartSession { mode_id }).unwrap();
    let mut sc = h.sidecar().await;
    sc.ready().await;
    let _ = sc.command().await;
    sc.send(SidecarEvent::Started {
        t0_host_ns: 1,
        emitted_at_ms: 0,
    })
    .await;
    h.wait(|s| s.phase == Phase::Listening).await;
    sc
}

async fn ask(h: &mut Harness, text: &str) -> CompletionRequest {
    let before = h.manual.calls().len();
    h.app
        .dispatch(Intent::Ask {
            text: text.into(),
            with_screenshot: false,
        })
        .unwrap();
    let manual = Arc::clone(&h.manual);
    h.wait(|s| state(s) == Some(AnswerState::Done) && manual.calls().len() > before)
        .await;
    h.manual.calls().remove(before)
}

fn notices(rx: &mut broadcast::Receiver<Notice>) -> Vec<Notice> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

async fn until(mut cond: impl FnMut() -> bool) {
    for _ in 0..500 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached");
}

#[tokio::test]
async fn modes_load_from_the_database_by_name() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    let s = h.wait(|s| s.modes.len() == 2).await;
    assert_eq!(s.modes[0].name, "Handbook");
    assert_eq!(s.modes[1].name, "Support");
    let small = mode(&s, SMALL);
    assert_eq!(small.system_prompt, "Answer like a support agent.");
    assert_eq!(small.files.len(), 1);
    assert!(!small.retrieval);
    assert_eq!(small.files[0].status, ModeFileStatus::Ready);
    let big = mode(&s, BIG);
    assert!(big.retrieval);
    assert_eq!(big.total_tokens, 40_000);
    assert_eq!(big.files[0].status, ModeFileStatus::Ready);
    assert_eq!(s.selected_mode_id, None);
}

#[tokio::test]
async fn create_update_delete_with_tombstones() {
    let mut h = harness(Opts::default());
    h.app
        .dispatch(Intent::CreateMode {
            name: "  Sales  ".into(),
            system_prompt: "Be brief.".into(),
        })
        .unwrap();
    let s = h.wait(|s| s.modes.len() == 1).await;
    let id = s.modes[0].id;
    assert_eq!(s.modes[0].name, "Sales");

    h.app
        .dispatch(Intent::UpdateMode {
            mode_id: id,
            name: "Discovery".into(),
            system_prompt: "Ask questions.".into(),
        })
        .unwrap();
    let s = h
        .wait(|s| s.modes.first().is_some_and(|m| m.name == "Discovery"))
        .await;
    assert_eq!(s.modes[0].system_prompt, "Ask questions.");

    h.app
        .dispatch(Intent::SelectMode { mode_id: Some(id) })
        .unwrap();
    h.wait(|s| s.selected_mode_id == Some(id)).await;
    h.app.dispatch(Intent::DeleteMode { mode_id: id }).unwrap();
    let s = h.wait(|s| s.modes.is_empty()).await;
    assert_eq!(s.selected_mode_id, None);

    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    assert!(db.list_modes().unwrap().is_empty());
    assert_eq!(
        db.get_setting("selected_mode").unwrap(),
        Some(serde_json::Value::Null)
    );
}

#[tokio::test]
async fn there_is_a_limit_on_modes() {
    let mut h = harness(Opts::default());
    let mut rx = h.app.notices();
    for i in 0..=super::super::modes::MAX_MODES {
        h.app
            .dispatch(Intent::CreateMode {
                name: format!("Mode {i}"),
                system_prompt: String::new(),
            })
            .unwrap();
    }
    let s = h
        .wait(|s| {
            s.notice
                .as_deref()
                .is_some_and(|n| n.contains("up to 50 modes"))
        })
        .await;
    assert_eq!(s.modes.len(), super::super::modes::MAX_MODES);
    assert!(notices(&mut rx).iter().any(|n| n.kind == NoticeKind::Modes));
}

#[tokio::test]
async fn the_selected_mode_is_saved_and_used_by_the_next_session() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    h.wait(|s| s.modes.len() == 2).await;
    h.app
        .dispatch(Intent::SelectMode {
            mode_id: Some(SMALL),
        })
        .unwrap();
    h.wait(|s| s.selected_mode_id == Some(SMALL)).await;
    let _sc = start_with(&mut h, None).await;
    let s = h.snaps.borrow().clone();
    assert_eq!(s.session.unwrap().mode_name.as_deref(), Some("Support"));
    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    assert_eq!(db.list_sessions(5).unwrap()[0].mode_id, Some(SMALL));
    assert_eq!(
        db.get_setting("selected_mode").unwrap(),
        Some(serde_json::json!(SMALL.to_string()))
    );
}

#[tokio::test]
async fn start_session_honors_the_requested_mode_over_the_selected_one() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    h.wait(|s| s.modes.len() == 2).await;
    h.app
        .dispatch(Intent::SelectMode {
            mode_id: Some(SMALL),
        })
        .unwrap();
    h.wait(|s| s.selected_mode_id == Some(SMALL)).await;
    let _sc = start_with(&mut h, Some(BIG)).await;
    let s = h.snaps.borrow().clone();
    assert_eq!(s.session.unwrap().mode_name.as_deref(), Some("Handbook"));
    assert_eq!(s.selected_mode_id, Some(SMALL));
}

#[tokio::test]
async fn an_unknown_mode_starts_a_plain_session() {
    let mut h = harness(Opts::default());
    let _sc = start_with(&mut h, Some(Uuid::from_u128(77))).await;
    assert_eq!(h.snaps.borrow().session.as_ref().unwrap().mode_name, None);
    let req = ask(&mut h, "hello there").await;
    assert_eq!(req.mode_context, "");
}

#[tokio::test]
async fn a_small_mode_puts_instructions_and_full_text_in_the_prompt() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    h.wait(|s| s.modes.len() == 2).await;
    let _sc = start_with(&mut h, Some(SMALL)).await;
    let req = ask(&mut h, "Can I get a refund?").await;
    assert!(req.mode_context.starts_with("Meeting context:\nInstructions for this meeting, from the user:\nAnswer like a support agent."));
    assert!(
        req.mode_context
            .contains("<file name=\"faq.md\">\nRefunds are allowed within 30 days.\n</file>")
    );
    // Full text needs no embedding at all.
    assert_eq!(fake.embed_calls.load(Ordering::SeqCst), 0);
    // The same mode gives the same bytes every time, so the prefix can be cached.
    let again = ask(&mut h, "And exchanges?").await;
    assert_eq!(again.mode_context, req.mode_context);
}

#[tokio::test]
async fn a_large_mode_retrieves_only_the_matching_passages() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    h.wait(|s| s.modes.len() == 2).await;
    let _sc = start_with(&mut h, Some(BIG)).await;
    let req = ask(&mut h, "What is the refund window?").await;
    assert!(req.mode_context.contains("Use the handbook."));
    // Passages change with the question, so they stay out of the cached prefix.
    assert!(!req.mode_context.contains(PASSAGES[0]));
    assert!(req.user_text.contains("match the current question"));
    assert!(req.user_text.contains(PASSAGES[0]), "{}", req.user_text);
    assert!(req.user_text.ends_with("What is the refund window?"));
    assert!(!req.user_text.contains(PASSAGES[1]));
    assert!(!req.user_text.contains(PASSAGES[2]));
    assert!(!req.mode_context.contains(HEAD) && !req.user_text.contains(HEAD));
    let other = ask(&mut h, "What are the support hours?").await;
    assert_eq!(
        other.mode_context, req.mode_context,
        "the cached prefix is the same for every question"
    );
    assert!(other.user_text.contains(PASSAGES[2]));
}

#[tokio::test]
async fn the_embedding_of_a_question_is_computed_once() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    h.wait(|s| s.modes.len() == 2).await;
    let _sc = start_with(&mut h, Some(BIG)).await;
    ask(&mut h, "What is the refund window?").await;
    let after_first = fake.embed_calls.load(Ordering::SeqCst);
    ask(&mut h, "What is the refund window?").await;
    assert_eq!(fake.embed_calls.load(Ordering::SeqCst), after_first);
    ask(&mut h, "When is support open?").await;
    assert_eq!(fake.embed_calls.load(Ordering::SeqCst), after_first + 1);
}

#[tokio::test]
async fn unavailable_embedder_falls_back_to_file_heads_with_one_notice() {
    let fake = Arc::new(FakeEmbedder::default());
    fake.fail_verify(Some(EmbedError::Unreachable));
    let mut h = harness(with_modes(&fake));
    let mut rx = h.app.notices();
    h.wait(|s| s.modes.len() == 2).await;
    let _sc = start_with(&mut h, Some(BIG)).await;
    let req = ask(&mut h, "What is the refund window?").await;
    assert!(req.mode_context.contains(HEAD));
    assert!(
        !req.mode_context.contains(TAIL),
        "the fallback is truncated"
    );
    assert!(
        req.mode_context
            .contains("only the start of each file is shown")
    );
    assert!(req.mode_context.contains("Use the handbook."));
    ask(&mut h, "Another question about support hours").await;
    let got: Vec<_> = notices(&mut rx)
        .into_iter()
        .filter(|n| n.kind == NoticeKind::Modes)
        .collect();
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].message.contains("Ollama is not running"));
    assert!(!got[0].message.contains(HEAD));
}

#[tokio::test]
async fn a_digest_mismatch_refuses_the_model() {
    let fake = Arc::new(FakeEmbedder::default());
    fake.fail_verify(Some(EmbedError::DigestMismatch {
        name: "fake-embed".into(),
        expected: "aaaaaaaaaaaa".into(),
        found: "bbbbbbbbbbbb".into(),
    }));
    let mut h = harness(with_modes(&fake));
    let mut rx = h.app.notices();
    h.wait(|s| s.modes.len() == 2).await;
    let _sc = start_with(&mut h, Some(BIG)).await;
    let req = ask(&mut h, "What is the refund window?").await;
    assert!(req.mode_context.contains(HEAD));
    assert_eq!(
        fake.embed_calls.load(Ordering::SeqCst),
        0,
        "a refused model must not be called"
    );
    let got = notices(&mut rx);
    assert!(
        got.iter()
            .any(|n| n.message.contains("not the version Sotto is pinned to")
                && n.message.contains("bbbbbbbbbbbb"))
    );
}

#[tokio::test]
async fn a_failing_query_embedding_falls_back_without_failing_the_answer() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    h.wait(|s| s.modes.len() == 2).await;
    let _sc = start_with(&mut h, Some(BIG)).await;
    until(|| fake.embed_calls.load(Ordering::SeqCst) >= 1).await;
    fake.fail_embed(Some(EmbedError::Status(500)));
    let req = ask(&mut h, "What is the refund window?").await;
    assert!(req.mode_context.contains(HEAD));
    assert!(!req.mode_context.contains(TAIL));
}

#[tokio::test]
async fn a_slow_embedder_does_not_hold_the_answer_back() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    h.wait(|s| s.modes.len() == 2).await;
    let _sc = start_with(&mut h, Some(BIG)).await;
    until(|| fake.embed_calls.load(Ordering::SeqCst) >= 1).await;
    fake.set_delay(Duration::from_secs(5));
    let started = std::time::Instant::now();
    let req = ask(&mut h, "What is the refund window?").await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(req.mode_context.contains(HEAD));
}

#[tokio::test]
async fn automatic_answers_get_the_same_mode_context() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    h.wait(|s| s.modes.len() == 2).await;
    let sc = start_with(&mut h, Some(BIG)).await;
    sc.them(1, 0, 0, 500, "como voce").await;
    sc.them(1, 1, 0, 1_000, "what is the refund window for annual plans")
        .await;
    h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    let calls = h.answer.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].user_text.contains(PASSAGES[0]));
    assert!(!calls[0].user_text.contains(PASSAGES[1]));
    // The classifier call never carries mode text.
    assert!(
        h.classifier
            .calls()
            .iter()
            .all(|c| c.mode_context.is_empty())
    );
}

fn write_file(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.path().join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

async fn new_mode(h: &mut Harness, name: &str) -> Uuid {
    h.app
        .dispatch(Intent::CreateMode {
            name: name.into(),
            system_prompt: "Use the files.".into(),
        })
        .unwrap();
    h.wait(|s| s.modes.iter().any(|m| m.name == name))
        .await
        .modes
        .iter()
        .find(|m| m.name == name)
        .unwrap()
        .id
}

#[tokio::test]
async fn files_are_added_checked_and_listed() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    let mut rx = h.app.notices();
    let id = new_mode(&mut h, "Docs").await;
    let files = tempfile::tempdir().unwrap();
    let paths = vec![
        write_file(&files, "notes.txt", b"Plain notes about onboarding."),
        write_file(&files, "guide.md", b"# Guide\n\nSteps for setup."),
        write_file(
            &files,
            "spec.pdf",
            &sotto_rag::extract::fixtures::pdf(&["Spec text about billing"]),
        ),
        write_file(&files, "sheet.xlsx", b"nope"),
        write_file(&files, "empty.txt", b"   "),
        write_file(&files, "broken.pdf", b"not a pdf"),
    ];
    h.app.add_mode_files(id, paths).unwrap();
    let s = h
        .wait(|s| s.modes.iter().any(|m| m.id == id && m.files.len() == 3))
        .await;
    let names: Vec<_> = mode(&s, id).files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["guide.md", "notes.txt", "spec.pdf"]);
    assert!(
        mode(&s, id)
            .files
            .iter()
            .all(|f| f.tokens > 0 && f.status == ModeFileStatus::Ready)
    );
    assert!(!mode(&s, id).retrieval);

    tokio::time::sleep(Duration::from_millis(100)).await;
    let messages: Vec<String> = notices(&mut rx).into_iter().map(|n| n.message).collect();
    assert!(
        messages
            .iter()
            .any(|m| m == "sheet.xlsx: only .txt, .md and .pdf files can be used"),
        "{messages:?}"
    );
    assert!(messages.iter().any(|m| m == "empty.txt: it has no text"));
    assert!(
        messages
            .iter()
            .any(|m| m == "broken.pdf: the PDF could not be read")
    );

    let _sc = start_with(&mut h, Some(id)).await;
    let req = ask(&mut h, "How do I set up billing?").await;
    assert!(req.mode_context.contains("Spec text about billing"));
    assert!(req.mode_context.contains("Steps for setup."));
}

#[tokio::test]
async fn a_file_over_ten_megabytes_is_rejected() {
    let mut h = harness(Opts::default());
    let mut rx = h.app.notices();
    let id = new_mode(&mut h, "Docs").await;
    let files = tempfile::tempdir().unwrap();
    let big = vec![b'a'; 10 * 1024 * 1024 + 1];
    h.app
        .add_mode_files(id, vec![write_file(&files, "big.txt", &big)])
        .unwrap();
    h.wait(|s| {
        s.notice
            .as_deref()
            .is_some_and(|n| n.contains("larger than 10 MB"))
    })
    .await;
    assert!(mode(&h.snaps.borrow().clone(), id).files.is_empty());
    assert!(notices(&mut rx).iter().all(|n| n.kind == NoticeKind::Modes));
}

#[tokio::test]
async fn a_mode_has_a_file_count_limit() {
    let mut h = harness(Opts::default());
    let id = new_mode(&mut h, "Docs").await;
    let files = tempfile::tempdir().unwrap();
    let limit = super::super::mode_files::MAX_FILES_PER_MODE;
    let paths: Vec<PathBuf> = (0..limit + 3)
        .map(|i| {
            write_file(
                &files,
                &format!("f{i:02}.txt"),
                format!("text {i}").as_bytes(),
            )
        })
        .collect();
    h.app.add_mode_files(id, paths).unwrap();
    let s = h
        .wait(|s| s.modes.first().is_some_and(|m| m.files.len() == limit))
        .await;
    assert_eq!(mode(&s, id).files.len(), limit);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(mode(&h.snaps.borrow().clone(), id).files.len(), limit);
}

#[tokio::test]
async fn adding_a_file_with_the_same_name_replaces_it() {
    let mut h = harness(Opts::default());
    let id = new_mode(&mut h, "Docs").await;
    let files = tempfile::tempdir().unwrap();
    let path = write_file(&files, "faq.txt", b"first version");
    h.app.add_mode_files(id, vec![path.clone()]).unwrap();
    let first = h
        .wait(|s| s.modes.first().is_some_and(|m| m.files.len() == 1))
        .await;
    let old_id = mode(&first, id).files[0].id;
    std::fs::write(&path, b"second version with more words").unwrap();
    h.app.add_mode_files(id, vec![path]).unwrap();
    let s = h
        .wait(|s| {
            s.modes
                .first()
                .is_some_and(|m| m.files.first().is_some_and(|f| f.id != old_id))
        })
        .await;
    assert_eq!(mode(&s, id).files.len(), 1);
    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    let files = db.list_mode_files(id).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].text, "second version with more words");
}

fn large_document() -> String {
    let mut paragraphs: Vec<String> = (0..2_500)
        .map(|i| {
            format!("Paragraph {i} says something about topic {i} in the handbook for new staff.")
        })
        .collect();
    paragraphs[1_700] =
        "Zebra crossing regulations require drivers to stop for pedestrians. ".repeat(8);
    paragraphs.join("\n\n")
}

#[tokio::test]
async fn a_large_file_is_indexed_in_the_background_and_retrieved() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    let id = new_mode(&mut h, "Big docs").await;
    let files = tempfile::tempdir().unwrap();
    let text = large_document();
    assert!(text.len() / 4 > 30_000);
    h.app
        .add_mode_files(
            id,
            vec![write_file(&files, "handbook.txt", text.as_bytes())],
        )
        .unwrap();
    let s = h
        .wait(|s| s.modes.iter().any(|m| m.id == id && m.retrieval))
        .await;
    assert_ne!(mode(&s, id).files[0].status, ModeFileStatus::Failed);
    let s = h
        .wait(|s| {
            s.modes.iter().any(|m| {
                m.id == id
                    && m.files
                        .first()
                        .is_some_and(|f| f.status == ModeFileStatus::Ready)
            })
        })
        .await;
    assert!(mode(&s, id).retrieval);

    let _sc = start_with(&mut h, Some(id)).await;
    let req = ask(&mut h, "What do the zebra crossing regulations say?").await;
    assert!(
        req.user_text
            .contains("Zebra crossing regulations require drivers to stop")
    );
    assert!(
        estimate_tokens_of(&req.mode_context) + estimate_tokens_of(&req.user_text) < 5_000,
        "retrieval keeps the prompt small"
    );

    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    assert!(db.mode_chunks(id).unwrap().len() > 100);
    assert_eq!(
        db.get_setting("rag_embedding").unwrap(),
        Some(identity_of(&fake))
    );
}

fn estimate_tokens_of(s: &str) -> usize {
    s.len() / 4
}

#[tokio::test]
async fn indexing_progress_is_visible_in_the_snapshot() {
    let fake = Arc::new(FakeEmbedder::default());
    fake.set_delay(Duration::from_millis(30));
    let mut h = harness(with_modes(&fake));
    let id = new_mode(&mut h, "Big docs").await;
    let files = tempfile::tempdir().unwrap();
    h.app
        .add_mode_files(
            id,
            vec![write_file(
                &files,
                "handbook.txt",
                large_document().as_bytes(),
            )],
        )
        .unwrap();
    let s = h
        .wait(|s| {
            s.modes.iter().any(|m| {
                m.id == id
                    && m.files
                        .first()
                        .is_some_and(|f| f.status == ModeFileStatus::Indexing && f.chunks_done > 0)
            })
        })
        .await;
    let f = &mode(&s, id).files[0];
    assert!(f.chunks_total > f.chunks_done);
    h.wait(|s| {
        s.modes.iter().any(|m| {
            m.id == id
                && m.files
                    .first()
                    .is_some_and(|f| f.status == ModeFileStatus::Ready)
        })
    })
    .await;
}

#[tokio::test]
async fn indexing_fails_visibly_when_the_model_is_unavailable_and_retries() {
    let fake = Arc::new(FakeEmbedder::default());
    fake.fail_verify(Some(EmbedError::ModelMissing {
        name: "fake-embed".into(),
    }));
    let mut h = harness(with_modes(&fake));
    let id = new_mode(&mut h, "Big docs").await;
    let files = tempfile::tempdir().unwrap();
    h.app
        .add_mode_files(
            id,
            vec![write_file(
                &files,
                "handbook.txt",
                large_document().as_bytes(),
            )],
        )
        .unwrap();
    let s = h
        .wait(|s| {
            s.modes.iter().any(|m| {
                m.id == id
                    && m.files
                        .first()
                        .is_some_and(|f| f.status == ModeFileStatus::Failed)
            })
        })
        .await;
    let error = mode(&s, id).files[0].error.clone().unwrap();
    assert!(error.contains("not installed"), "{error}");
    assert!(s.notice.unwrap().contains("press Retry"));

    fake.fail_verify(None);
    h.app
        .dispatch(Intent::RetryIndexing { mode_id: id })
        .unwrap();
    let s = h
        .wait(|s| {
            s.modes.iter().any(|m| {
                m.id == id
                    && m.files
                        .first()
                        .is_some_and(|f| f.status == ModeFileStatus::Ready)
            })
        })
        .await;
    assert_eq!(mode(&s, id).files[0].error, None);
}

#[tokio::test]
async fn a_batch_error_marks_the_file_failed() {
    let fake = Arc::new(FakeEmbedder::default());
    fake.fail_embed(Some(EmbedError::Status(500)));
    let mut h = harness(with_modes(&fake));
    let id = new_mode(&mut h, "Big docs").await;
    let files = tempfile::tempdir().unwrap();
    h.app
        .add_mode_files(
            id,
            vec![write_file(
                &files,
                "handbook.txt",
                large_document().as_bytes(),
            )],
        )
        .unwrap();
    let s = h
        .wait(|s| {
            s.modes.iter().any(|m| {
                m.id == id
                    && m.files
                        .first()
                        .is_some_and(|f| f.status == ModeFileStatus::Failed)
            })
        })
        .await;
    assert_eq!(
        mode(&s, id).files[0].error.as_deref(),
        Some("Ollama answered with status 500")
    );
}

#[tokio::test]
async fn vectors_from_another_model_are_dropped_and_recomputed() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(Opts {
        seed_mut: Some(seed_stale_index),
        embedder: Some(Arc::clone(&fake) as Arc<dyn sotto_rag::Embedder>),
        ..Opts::default()
    });
    h.wait(|s| s.modes.len() == 2).await;
    // The seeded file is large enough for retrieval and has no usable vectors, so it is indexed
    // again with the current model.
    until(|| fake.inputs_embedded.load(Ordering::SeqCst) > 0).await;
    h.wait(|s| {
        s.modes.iter().any(|m| {
            m.id == BIG
                && m.files
                    .first()
                    .is_some_and(|f| f.status == ModeFileStatus::Ready)
        })
    })
    .await;
    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    let chunks = db.mode_chunks(BIG).unwrap();
    assert!(
        chunks.len() > 10,
        "the old three passages were replaced by the real chunks"
    );
    assert!(chunks.iter().all(|c| !PASSAGES.contains(&c.text.as_str())));
    assert_eq!(
        db.get_setting("rag_embedding").unwrap(),
        Some(identity_of(&fake))
    );
}

#[tokio::test]
async fn removing_a_file_erases_its_text_and_vectors() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    let s = h.wait(|s| s.modes.len() == 2).await;
    let file_id = mode(&s, BIG).files[0].id;
    h.app
        .dispatch(Intent::RemoveModeFile {
            mode_id: BIG,
            file_id,
        })
        .unwrap();
    let s = h
        .wait(|s| s.modes.iter().any(|m| m.id == BIG && m.files.is_empty()))
        .await;
    assert_eq!(mode(&s, BIG).total_tokens, 0);
    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    assert!(db.list_mode_files(BIG).unwrap().is_empty());
    assert!(db.mode_chunks(BIG).unwrap().is_empty());
    assert!(db.mode_file_text(file_id).unwrap().is_none());
}

#[tokio::test]
async fn deleting_a_mode_keeps_a_session_that_uses_it_running() {
    let fake = Arc::new(FakeEmbedder::default());
    let mut h = harness(with_modes(&fake));
    h.wait(|s| s.modes.len() == 2).await;
    let _sc = start_with(&mut h, Some(SMALL)).await;
    h.app
        .dispatch(Intent::DeleteMode { mode_id: SMALL })
        .unwrap();
    h.wait(|s| s.modes.len() == 1).await;
    let req = ask(&mut h, "Can I get a refund?").await;
    assert!(
        req.mode_context
            .contains("Refunds are allowed within 30 days.")
    );
    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    assert_eq!(db.list_sessions(5).unwrap()[0].mode_id, Some(SMALL));
    assert!(db.list_mode_files(SMALL).unwrap().is_empty());
}

#[tokio::test]
async fn selecting_an_unknown_mode_is_ignored() {
    let mut h = harness(Opts::default());
    h.app
        .dispatch(Intent::SelectMode {
            mode_id: Some(Uuid::from_u128(5)),
        })
        .unwrap();
    h.app
        .dispatch(Intent::SetAutoAnswer { enabled: false })
        .unwrap();
    h.wait(|s| !s.auto_answer).await;
    assert_eq!(h.snaps.borrow().selected_mode_id, None);
}
