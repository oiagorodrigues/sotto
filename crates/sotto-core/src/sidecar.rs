//! The `sotto-audio` sidecar process (ADR 0007): JSON lines on stdin and stdout, diagnostics on
//! stderr. The binary is always launched by the absolute path the caller gives; `PATH` is never
//! consulted or modified (security checklist item 9).

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use command_fds::{CommandFdExt, FdMapping};
use sotto_protocol::{SidecarCommand, SidecarEvent};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::cloud_stt::{PcmFrame, pump_frames};

/// Screenshots travel as base64 inside one JSON line, so lines can be large.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

const EVENT_CHANNEL_CAPACITY: usize = 256;
const PCM_CHANNEL_CAPACITY: usize = 512;
/// Where the sidecar expects the raw PCM pipe (ADR 0007).
const PCM_CHILD_FD: i32 = 3;
const STDIN_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error("sidecar path must be absolute: {0}")]
    NotAbsolute(PathBuf),
    #[error("cannot start sidecar: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("cannot write to sidecar stdin: {0}")]
    Write(#[source] std::io::Error),
    #[error("writing to sidecar stdin timed out")]
    WriteTimeout,
    #[error("cannot encode command: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("sidecar stdin is closed")]
    StdinClosed,
}

pub type PcmReceiver = mpsc::Receiver<PcmFrame>;

/// Everything the reader tasks report on the event channel.
#[derive(Debug, Clone, PartialEq)]
pub enum SidecarOutput {
    Event(SidecarEvent),
    /// A stdout line that is not a valid event. The line content is not included because it
    /// may hold transcript text.
    BadLine {
        reason: String,
    },
    /// The process ended. Always the last item; every event it wrote precedes it.
    Exited {
        code: Option<i32>,
        success: bool,
    },
}

#[derive(Debug)]
pub struct SidecarProcess {
    stdin: Option<ChildStdin>,
    pid: Option<u32>,
    kill: CancellationToken,
    waiter: Option<JoinHandle<()>>,
}

impl SidecarProcess {
    /// Starts the sidecar with `--exclude-pid <our pid>` followed by `args`.
    pub fn spawn(
        path: &Path,
        args: &[String],
    ) -> Result<(Self, mpsc::Receiver<SidecarOutput>), SidecarError> {
        let (process, outputs, _) = Self::spawn_inner(path, args, false)?;
        Ok((process, outputs))
    }

    /// Like [`spawn`](Self::spawn), and also opens the pipe the sidecar writes raw PCM frames to
    /// as file descriptor 3, for the cloud engine.
    pub fn spawn_with_pcm(
        path: &Path,
        args: &[String],
    ) -> Result<(Self, mpsc::Receiver<SidecarOutput>, PcmReceiver), SidecarError> {
        let (process, outputs, pcm) = Self::spawn_inner(path, args, true)?;
        let pcm = pcm.ok_or_else(|| SidecarError::Spawn(std::io::Error::other("no PCM pipe")))?;
        Ok((process, outputs, pcm))
    }

    fn spawn_inner(
        path: &Path,
        args: &[String],
        with_pcm: bool,
    ) -> Result<(Self, mpsc::Receiver<SidecarOutput>, Option<PcmReceiver>), SidecarError> {
        if !path.is_absolute() {
            return Err(SidecarError::NotAbsolute(path.to_path_buf()));
        }
        let mut command = Command::new(path);
        command
            .arg("--exclude-pid")
            .arg(std::process::id().to_string())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let pcm_reader = if with_pcm {
            let (reader, writer) = std::io::pipe().map_err(SidecarError::Spawn)?;
            command
                .fd_mappings(vec![FdMapping {
                    parent_fd: OwnedFd::from(writer),
                    child_fd: PCM_CHILD_FD,
                }])
                .map_err(|e| SidecarError::Spawn(std::io::Error::other(e.to_string())))?;
            Some(reader)
        } else {
            None
        };
        let spawned = command.spawn().map_err(SidecarError::Spawn);
        // The command holds the parent's copy of the write end; closing it lets the reader see
        // end of stream when the sidecar exits.
        drop(command);
        let mut child = spawned?;
        let pcm = pcm_reader
            .map(|reader| {
                let file = std::fs::File::from(OwnedFd::from(reader));
                tokio::net::unix::pipe::Receiver::from_file(file).map_err(SidecarError::Spawn)
            })
            .transpose()?
            .map(|receiver| {
                let (tx, rx) = mpsc::channel(PCM_CHANNEL_CAPACITY);
                tokio::spawn(pump_frames(receiver, tx));
                rx
            });

        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

        let reader = stdout.map(|out| tokio::spawn(read_events(BufReader::new(out), tx.clone())));
        if let Some(err) = stderr {
            tokio::spawn(forward_stderr(BufReader::new(err)));
        }

        let kill = CancellationToken::new();
        let waiter = tokio::spawn(wait_for_exit(child, kill.clone(), reader, tx));
        tracing::debug!(?pid, "sidecar started");
        Ok((
            Self {
                stdin,
                pid,
                kill,
                waiter: Some(waiter),
            },
            rx,
            pcm,
        ))
    }

    /// Test and benchmark mode: transcribes a WAV file as the `them` speaker.
    pub fn spawn_file_mode(
        path: &Path,
        wav: &Path,
        locale: &str,
    ) -> Result<(Self, mpsc::Receiver<SidecarOutput>), SidecarError> {
        Self::spawn_file_mode_with(path, wav, locale, false)
    }

    /// File mode, optionally fed at real-time pace (`--realtime`) so latencies are comparable
    /// to a live call.
    pub fn spawn_file_mode_with(
        path: &Path,
        wav: &Path,
        locale: &str,
        realtime: bool,
    ) -> Result<(Self, mpsc::Receiver<SidecarOutput>), SidecarError> {
        let mut args = vec![
            "--file".to_string(),
            wav.to_string_lossy().into_owned(),
            "--speaker".to_string(),
            "them".to_string(),
            "--locale".to_string(),
            locale.to_string(),
        ];
        if realtime {
            args.push("--realtime".to_string());
        }
        Self::spawn(path, &args)
    }

    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub async fn send(&mut self, command: &SidecarCommand) -> Result<(), SidecarError> {
        let stdin = self.stdin.as_mut().ok_or(SidecarError::StdinClosed)?;
        let mut line = serde_json::to_vec(command)?;
        line.push(b'\n');
        let write = async {
            stdin.write_all(&line).await?;
            stdin.flush().await
        };
        match tokio::time::timeout(STDIN_WRITE_TIMEOUT, write).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(SidecarError::Write(e)),
            Err(_) => Err(SidecarError::WriteTimeout),
        }
    }

    /// Asks the sidecar to shut down, waits up to `grace` for it to exit, then kills it.
    pub async fn shutdown(mut self, grace: Duration) {
        let _ = self.send(&SidecarCommand::Shutdown {}).await;
        self.stdin = None;
        if let Some(waiter) = self.waiter.take()
            && tokio::time::timeout(grace, waiter).await.is_err()
        {
            tracing::debug!("sidecar ignored shutdown, killing");
        }
        self.kill.cancel();
    }
}

impl Drop for SidecarProcess {
    fn drop(&mut self) {
        self.kill.cancel();
    }
}

async fn wait_for_exit(
    mut child: Child,
    kill: CancellationToken,
    reader: Option<JoinHandle<()>>,
    tx: mpsc::Sender<SidecarOutput>,
) {
    let status: Option<ExitStatus> = tokio::select! {
        () = kill.cancelled() => {
            let _ = child.kill().await;
            child.wait().await.ok()
        }
        s = child.wait() => s.ok(),
    };
    if let Some(reader) = reader {
        // Let the reader deliver what the process wrote before it died.
        let _ = tokio::time::timeout(READER_DRAIN_TIMEOUT, reader).await;
    }
    let (code, success) = status.map_or((None, false), |s| (s.code(), s.success()));
    tracing::debug!(?code, "sidecar exited");
    let _ = tx.send(SidecarOutput::Exited { code, success }).await;
}

async fn forward_stderr<R: AsyncBufRead + Unpin>(mut reader: R) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => tracing::debug!(target: "sotto_audio", "{}", line.trim_end()),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LineRead {
    Line,
    TooLong,
    Eof,
}

/// Reads one `\n`-terminated line into `buf`. A line over `max` bytes is consumed and
/// discarded up to its newline, so one oversized line cannot desynchronise the stream or
/// exhaust memory.
async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<LineRead> {
    buf.clear();
    let mut too_long = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if too_long {
                LineRead::TooLong
            } else if buf.is_empty() {
                LineRead::Eof
            } else {
                LineRead::Line
            });
        }
        let (take, found_newline) = match available.iter().position(|b| *b == b'\n') {
            Some(pos) => (pos + 1, true),
            None => (available.len(), false),
        };
        if !too_long {
            if buf.len() + take > max + 1 {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(&available[..take]);
            }
        }
        reader.consume(take);
        if found_newline {
            if too_long {
                return Ok(LineRead::TooLong);
            }
            buf.pop();
            return Ok(LineRead::Line);
        }
    }
}

async fn read_events<R: AsyncBufRead + Unpin>(mut reader: R, tx: mpsc::Sender<SidecarOutput>) {
    let mut buf = Vec::new();
    loop {
        let out = match read_bounded_line(&mut reader, &mut buf, MAX_LINE_BYTES).await {
            Ok(LineRead::Eof) => return,
            Err(e) => {
                tracing::debug!(error = %e, "sidecar stdout read failed");
                return;
            }
            Ok(LineRead::TooLong) => SidecarOutput::BadLine {
                reason: format!("line longer than {MAX_LINE_BYTES} bytes"),
            },
            Ok(LineRead::Line) => {
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
                if buf.is_empty() {
                    continue;
                }
                match serde_json::from_slice::<SidecarEvent>(&buf) {
                    Ok(event) => SidecarOutput::Event(event),
                    Err(e) => SidecarOutput::BadLine {
                        reason: e.to_string(),
                    },
                }
            }
        };
        // A full channel blocks the reader, which backs up the pipe: slow, never lossy.
        if tx.send(out).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use sotto_protocol::{Speaker, sidecar::EngineKind};
    use tempfile::TempDir;

    use super::*;

    const READY: &str = r#"{"type":"ready","protocol_version":1,"sidecar_version":"fake","installed_locales":["pt-BR"]}"#;

    fn script(dir: &TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("fake-sidecar");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    async fn next(rx: &mut mpsc::Receiver<SidecarOutput>) -> SidecarOutput {
        tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap()
    }

    fn error_message(out: SidecarOutput) -> String {
        match out {
            SidecarOutput::Event(SidecarEvent::Error { message, .. }) => message,
            other => panic!("expected error event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_relative_paths() {
        let r = SidecarProcess::spawn(Path::new("sotto-audio"), &[]);
        assert!(matches!(r, Err(SidecarError::NotAbsolute(_))));
    }

    #[tokio::test]
    async fn passes_exclude_pid_and_extra_args() {
        let dir = tempfile::tempdir().unwrap();
        let sh = script(
            &dir,
            &format!(
                r#"echo '{READY}'; echo "{{\"type\":\"error\",\"code\":\"internal\",\"message\":\"$*\",\"fatal\":false}}""#
            ),
        );
        let (_p, mut rx) = SidecarProcess::spawn(&sh, &["--x".into(), "y".into()]).unwrap();
        assert!(matches!(
            next(&mut rx).await,
            SidecarOutput::Event(SidecarEvent::Ready { .. })
        ));
        let msg = error_message(next(&mut rx).await);
        assert_eq!(msg, format!("--exclude-pid {} --x y", std::process::id()));
    }

    #[tokio::test]
    async fn file_mode_builds_the_documented_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let sh = script(
            &dir,
            r#"echo "{\"type\":\"error\",\"code\":\"internal\",\"message\":\"$*\",\"fatal\":false}""#,
        );
        let (_p, mut rx) =
            SidecarProcess::spawn_file_mode(&sh, Path::new("/tmp/a.wav"), "pt-BR").unwrap();
        let msg = error_message(next(&mut rx).await);
        assert_eq!(
            msg,
            format!(
                "--exclude-pid {} --file /tmp/a.wav --speaker them --locale pt-BR",
                std::process::id()
            )
        );
    }

    #[tokio::test]
    async fn realtime_file_mode_adds_the_flag_last() {
        let dir = tempfile::tempdir().unwrap();
        let sh = script(
            &dir,
            r#"echo "{\"type\":\"error\",\"code\":\"internal\",\"message\":\"$*\",\"fatal\":false}""#,
        );
        let (_p, mut rx) =
            SidecarProcess::spawn_file_mode_with(&sh, Path::new("/tmp/a.wav"), "en-US", true)
                .unwrap();
        let msg = error_message(next(&mut rx).await);
        assert!(msg.ends_with("--locale en-US --realtime"), "{msg}");
    }

    #[tokio::test]
    async fn commands_reach_stdin_as_json_lines_and_exit_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let sh = script(
            &dir,
            &format!(
                r#"echo '{READY}'
read line
esc=$(printf '%s' "$line" | sed 's/"/\\"/g')
echo "{{\"type\":\"error\",\"code\":\"internal\",\"message\":\"$esc\",\"fatal\":false}}"
exit 3"#
            ),
        );
        let (mut p, mut rx) = SidecarProcess::spawn(&sh, &[]).unwrap();
        assert!(matches!(
            next(&mut rx).await,
            SidecarOutput::Event(SidecarEvent::Ready { .. })
        ));
        p.send(&SidecarCommand::Start {
            locale: "pt-BR".into(),
            engine: EngineKind::Local,
            speakers: vec![Speaker::Them],
        })
        .await
        .unwrap();
        let echoed = error_message(next(&mut rx).await);
        let cmd: SidecarCommand = serde_json::from_str(&echoed).unwrap();
        assert!(matches!(cmd, SidecarCommand::Start { .. }));
        assert_eq!(
            next(&mut rx).await,
            SidecarOutput::Exited {
                code: Some(3),
                success: false
            }
        );
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn bad_lines_are_reported_without_stopping_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let sh = script(
            &dir,
            &format!(
                r#"echo 'not json with secret words'; echo '{{"type":"stopped","extra":1}}'; echo '{READY}'"#
            ),
        );
        let (_p, mut rx) = SidecarProcess::spawn(&sh, &[]).unwrap();
        for _ in 0..2 {
            match next(&mut rx).await {
                SidecarOutput::BadLine { reason } => assert!(!reason.contains("secret")),
                other => panic!("expected bad line, got {other:?}"),
            }
        }
        assert!(matches!(
            next(&mut rx).await,
            SidecarOutput::Event(SidecarEvent::Ready { .. })
        ));
    }

    #[tokio::test]
    async fn large_screenshot_lines_pass_and_oversized_lines_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let sh = script(
            &dir,
            &format!(
                r#"printf '{{"type":"screenshot","request_id":"018f0000-0000-7000-8000-000000000000","png_base64":"'
head -c 3000000 /dev/zero | tr '\0' 'A'
printf '","width_px":1,"height_px":1}}\n'
head -c 17000000 /dev/zero | tr '\0' 'a'
echo
echo '{READY}'"#
            ),
        );
        let (_p, mut rx) = SidecarProcess::spawn(&sh, &[]).unwrap();
        match next(&mut rx).await {
            SidecarOutput::Event(SidecarEvent::Screenshot { png_base64, .. }) => {
                assert_eq!(png_base64.len(), 3_000_000);
            }
            other => panic!("expected screenshot, got {other:?}"),
        }
        assert!(matches!(next(&mut rx).await, SidecarOutput::BadLine { .. }));
        assert!(matches!(
            next(&mut rx).await,
            SidecarOutput::Event(SidecarEvent::Ready { .. })
        ));
    }

    #[tokio::test]
    async fn bounded_reader_handles_edge_cases() {
        let mut buf = Vec::new();
        let mut r = BufReader::new(&b"abc\nabcdefgh\nlast"[..]);
        assert_eq!(
            read_bounded_line(&mut r, &mut buf, 5).await.unwrap(),
            LineRead::Line
        );
        assert_eq!(buf, b"abc");
        assert_eq!(
            read_bounded_line(&mut r, &mut buf, 5).await.unwrap(),
            LineRead::TooLong
        );
        assert_eq!(
            read_bounded_line(&mut r, &mut buf, 5).await.unwrap(),
            LineRead::Line
        );
        assert_eq!(buf, b"last");
        assert_eq!(
            read_bounded_line(&mut r, &mut buf, 5).await.unwrap(),
            LineRead::Eof
        );
    }

    fn process_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn drop_kills_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let sh = script(
            &dir,
            r#"echo "{\"type\":\"error\",\"code\":\"internal\",\"message\":\"$$\",\"fatal\":false}"
exec sleep 60"#,
        );
        let (p, mut rx) = SidecarProcess::spawn(&sh, &[]).unwrap();
        let pid: u32 = error_message(next(&mut rx).await).parse().unwrap();
        assert!(process_alive(pid));
        drop(p);
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_alive(pid) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!process_alive(pid));
        assert!(matches!(
            next(&mut rx).await,
            SidecarOutput::Exited { success: false, .. }
        ));
    }

    #[tokio::test]
    async fn pcm_frames_arrive_from_file_descriptor_3_and_the_stream_ends_with_the_process() {
        let dir = tempfile::tempdir().unwrap();
        // Speaker them, start 0x0102 ms, two samples: 1 and -2.
        let sh = script(
            &dir,
            r"printf '\001\002\001\000\000\000\000\000\000\002\000\000\000\001\000\376\377' >&3
printf '\000\005\000\000\000\000\000\000\000\001\000\000\000\007\000' >&3",
        );
        let (_p, _outputs, mut pcm) = SidecarProcess::spawn_with_pcm(&sh, &[]).unwrap();
        let first = tokio::time::timeout(Duration::from_secs(10), pcm.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.speaker, Speaker::Them);
        assert_eq!(first.start_ms, 0x0102);
        assert_eq!(first.samples, vec![1, -2]);
        let second = pcm.recv().await.unwrap();
        assert_eq!(
            (second.speaker, second.start_ms, second.samples),
            (Speaker::Me, 5, vec![7])
        );
        // The script exits, the parent's copy of the write end is closed, so the reader sees EOF.
        assert!(
            tokio::time::timeout(Duration::from_secs(10), pcm.recv())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn plain_spawn_leaves_file_descriptor_3_closed() {
        let dir = tempfile::tempdir().unwrap();
        let sh = script(
            &dir,
            r#"if (echo x >&3) 2>/dev/null; then m=open; else m=closed; fi
echo "{\"type\":\"error\",\"code\":\"internal\",\"message\":\"$m\",\"fatal\":false}""#,
        );
        let (_p, mut rx) = SidecarProcess::spawn(&sh, &[]).unwrap();
        assert_eq!(error_message(next(&mut rx).await), "closed");
    }

    #[tokio::test]
    async fn shutdown_waits_for_a_clean_exit() {
        let dir = tempfile::tempdir().unwrap();
        let sh = script(&dir, "read line; echo '{\"type\":\"stopped\"}'; exit 0");
        let (p, mut rx) = SidecarProcess::spawn(&sh, &[]).unwrap();
        p.shutdown(Duration::from_secs(5)).await;
        assert!(matches!(
            next(&mut rx).await,
            SidecarOutput::Event(SidecarEvent::Stopped {})
        ));
        assert_eq!(
            next(&mut rx).await,
            SidecarOutput::Exited {
                code: Some(0),
                success: true
            }
        );
    }
}
