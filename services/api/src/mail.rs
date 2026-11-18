use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, header::ContentType},
};

#[derive(Clone, PartialEq, Eq)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub body: String,
}

impl std::fmt::Debug for Mail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mail").finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("mail delivery failed")]
pub struct MailError;

#[async_trait]
pub trait Mailer: Send + Sync + std::fmt::Debug {
    async fn send(&self, mail: Mail) -> Result<(), MailError>;
}

pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl std::fmt::Debug for SmtpMailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpMailer").finish_non_exhaustive()
    }
}

impl SmtpMailer {
    /// `smtp://host:port` is plain (Mailpit), `smtps://` is implicit TLS and `?tls=required`
    /// upgrades with STARTTLS.
    pub fn new(smtp_url: &str, from: &str) -> Result<Self, MailError> {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(smtp_url)
            .map_err(|_| MailError)?
            .build();
        Ok(Self {
            transport,
            from: from.parse().map_err(|_| MailError)?,
        })
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send(&self, mail: Mail) -> Result<(), MailError> {
        let message = Message::builder()
            .from(self.from.clone())
            .to(mail.to.parse().map_err(|_| MailError)?)
            .subject(mail.subject)
            .header(ContentType::TEXT_PLAIN)
            .body(mail.body)
            .map_err(|_| MailError)?;
        self.transport
            .send(message)
            .await
            .map(|_| ())
            .map_err(|_| MailError)
    }
}

/// Keeps every message in memory. For tests.
#[derive(Debug, Default)]
pub struct CaptureMailer {
    sent: std::sync::Mutex<Vec<Mail>>,
}

impl CaptureMailer {
    pub fn take(&self) -> Vec<Mail> {
        std::mem::take(&mut *self.sent.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

#[async_trait]
impl Mailer for CaptureMailer {
    async fn send(&self, mail: Mail) -> Result<(), MailError> {
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(mail);
        Ok(())
    }
}
