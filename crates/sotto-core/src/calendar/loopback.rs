//! The loopback redirect receiver for the OAuth flow (RFC 8252, section 7.3).
//!
//! It listens on 127.0.0.1 only, on a random port, and finishes with the first request whose
//! `state` matches. Requests with another state or path get an error page and are ignored, so
//! a stray local request cannot cancel the sign-in.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;

use super::error::CalendarError;

const CALLBACK_PATH: &str = "/callback";
const MAX_REQUEST_BYTES: usize = 16 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub struct Loopback {
    listener: TcpListener,
}

/// What Google sent back. The code is a secret until exchanged.
#[derive(Clone, PartialEq, Eq)]
pub struct Callback {
    pub code: String,
}

impl std::fmt::Debug for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Callback").finish_non_exhaustive()
    }
}

impl Loopback {
    pub async fn bind() -> Result<Self, CalendarError> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .map_err(|e| CalendarError::Listen(e.kind().to_string()))?;
        Ok(Self { listener })
    }

    pub fn port(&self) -> Result<u16, CalendarError> {
        self.listener
            .local_addr()
            .map(|a| a.port())
            .map_err(|e| CalendarError::Listen(e.kind().to_string()))
    }

    pub fn redirect_uri(&self) -> Result<String, CalendarError> {
        Ok(format!("http://127.0.0.1:{}{CALLBACK_PATH}", self.port()?))
    }

    /// Consumes the listener, so it is closed when this returns.
    pub async fn wait(self, state: &str, timeout: Duration) -> Result<Callback, CalendarError> {
        let deadline = Instant::now() + timeout;
        loop {
            let accepted = tokio::time::timeout_at(deadline, self.listener.accept())
                .await
                .map_err(|_| CalendarError::Timeout(timeout))?;
            let Ok((stream, _)) = accepted else { continue };
            match handle(stream, state).await {
                Outcome::Done(result) => return result,
                Outcome::Ignored => {}
            }
        }
    }
}

enum Outcome {
    Done(Result<Callback, CalendarError>),
    Ignored,
}

async fn handle(mut stream: TcpStream, state: &str) -> Outcome {
    let Some(target) = read_target(&mut stream).await else {
        respond(
            &mut stream,
            400,
            "Bad request",
            "Sotto could not read that request.",
        )
        .await;
        return Outcome::Ignored;
    };
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        respond(
            &mut stream,
            400,
            "Bad request",
            "Sotto could not read that request.",
        )
        .await;
        return Outcome::Ignored;
    };
    if url.path() != CALLBACK_PATH {
        respond(&mut stream, 404, "Not found", "Nothing here.").await;
        return Outcome::Ignored;
    }
    let mut code = None;
    let mut error = None;
    let mut got_state = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "state" => got_state = Some(v.into_owned()),
            _ => {}
        }
    }
    if !state_matches(got_state.as_deref(), state) {
        respond(
            &mut stream,
            400,
            "Sign-in failed",
            "This request did not come from your sign-in. Go back to Sotto and try again.",
        )
        .await;
        return Outcome::Ignored;
    }
    if let Some(error) = error {
        respond(
            &mut stream,
            200,
            "Sign-in cancelled",
            "Google did not grant access. Return to Sotto.",
        )
        .await;
        return Outcome::Done(Err(CalendarError::Denied(sanitize(&error))));
    }
    match code.filter(|c| !c.is_empty()) {
        Some(code) => {
            respond(
                &mut stream,
                200,
                "Sotto is connected",
                "You can close this tab and return to Sotto.",
            )
            .await;
            Outcome::Done(Ok(Callback { code }))
        }
        None => {
            respond(
                &mut stream,
                400,
                "Sign-in failed",
                "Google sent no code. Return to Sotto and try again.",
            )
            .await;
            Outcome::Ignored
        }
    }
}

/// Constant-time comparison; the state is the only thing that ties a callback to our request.
fn state_matches(got: Option<&str>, expected: &str) -> bool {
    let Some(got) = got else { return false };
    if got.len() != expected.len() {
        return false;
    }
    got.bytes()
        .zip(expected.bytes())
        .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn sanitize(error: &str) -> String {
    error
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' '))
        .take(60)
        .collect()
}

async fn read_target(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let read = async {
        let mut chunk = [0_u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(2).any(|w| w == b"\r\n") || buf.len() > MAX_REQUEST_BYTES {
                return Some(());
            }
        }
    };
    tokio::time::timeout(READ_TIMEOUT, read).await.ok()??;
    let line_end = buf.windows(2).position(|w| w == b"\r\n")?;
    let line = std::str::from_utf8(&buf[..line_end]).ok()?;
    let mut parts = line.split(' ');
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    target.starts_with('/').then(|| target.to_string())
}

async fn respond(stream: &mut TcpStream, status: u16, title: &str, message: &str) {
    let reason = if status == 200 { "OK" } else { "Error" };
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title></head><body style=\"font-family:-apple-system,sans-serif;margin:3rem\"><h1>{title}</h1><p>{message}</p></body></html>"
    );
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(port: u16, target: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn binds_only_to_the_ipv4_loopback_address() {
        let l = Loopback::bind().await.unwrap();
        let addr = l.listener.local_addr().unwrap();
        assert_eq!(addr.ip(), std::net::IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_ne!(addr.port(), 0);
        assert_eq!(
            l.redirect_uri().unwrap(),
            format!("http://127.0.0.1:{}/callback", addr.port())
        );
    }

    #[tokio::test]
    async fn accepts_the_matching_state_and_closes() {
        let l = Loopback::bind().await.unwrap();
        let port = l.port().unwrap();
        let wait = tokio::spawn(l.wait("good", Duration::from_secs(5)));
        let page = get(port, "/callback?state=good&code=4%2Fabc").await;
        assert!(page.starts_with("HTTP/1.1 200"));
        assert!(page.contains("return to Sotto"));
        assert_eq!(wait.await.unwrap().unwrap().code, "4/abc");
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }

    #[tokio::test]
    async fn rejects_a_wrong_state_and_keeps_waiting() {
        let l = Loopback::bind().await.unwrap();
        let port = l.port().unwrap();
        let wait = tokio::spawn(l.wait("good", Duration::from_secs(5)));
        let bad = get(port, "/callback?state=evil&code=stolen").await;
        assert!(bad.starts_with("HTTP/1.1 400"));
        let none = get(port, "/callback?code=stolen").await;
        assert!(none.starts_with("HTTP/1.1 400"));
        let other = get(port, "/favicon.ico").await;
        assert!(other.starts_with("HTTP/1.1 404"));
        assert!(!wait.is_finished());
        get(port, "/callback?state=good&code=real").await;
        assert_eq!(wait.await.unwrap().unwrap().code, "real");
    }

    #[tokio::test]
    async fn only_one_request_is_accepted() {
        let l = Loopback::bind().await.unwrap();
        let port = l.port().unwrap();
        let wait = tokio::spawn(l.wait("s", Duration::from_secs(5)));
        get(port, "/callback?state=s&code=first").await;
        assert_eq!(wait.await.unwrap().unwrap().code, "first");
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }

    #[tokio::test]
    async fn reports_a_google_error_for_the_matching_state_only() {
        let l = Loopback::bind().await.unwrap();
        let port = l.port().unwrap();
        let wait = tokio::spawn(l.wait("s", Duration::from_secs(5)));
        get(port, "/callback?state=other&error=access_denied").await;
        assert!(!wait.is_finished());
        get(port, "/callback?state=s&error=access_denied").await;
        assert!(matches!(
            wait.await.unwrap(),
            Err(CalendarError::Denied(e)) if e == "access_denied"
        ));
    }

    #[tokio::test]
    async fn times_out_and_closes() {
        let l = Loopback::bind().await.unwrap();
        let port = l.port().unwrap();
        let result = l.wait("s", Duration::from_millis(100)).await;
        assert!(matches!(result, Err(CalendarError::Timeout(_))));
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }

    #[tokio::test]
    async fn non_get_requests_are_ignored() {
        let l = Loopback::bind().await.unwrap();
        let port = l.port().unwrap();
        let wait = tokio::spawn(l.wait("s", Duration::from_secs(5)));
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(b"POST /callback?state=s&code=x HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        assert!(out.starts_with("HTTP/1.1 400"));
        assert!(!wait.is_finished());
        wait.abort();
    }

    #[test]
    fn state_comparison() {
        assert!(state_matches(Some("abc"), "abc"));
        assert!(!state_matches(Some("abd"), "abc"));
        assert!(!state_matches(Some("ab"), "abc"));
        assert!(!state_matches(None, "abc"));
    }
}
