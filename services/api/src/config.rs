use std::net::SocketAddr;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing environment variable {0}")]
    Missing(&'static str),
    #[error("invalid value for {0}")]
    Invalid(&'static str),
}

#[derive(Clone)]
pub enum RealtimeConfig {
    Fake,
    Openai {
        api_key: String,
        base_url: String,
        transcribe_model: String,
    },
}

impl std::fmt::Debug for RealtimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fake => f.write_str("Fake"),
            Self::Openai { base_url, .. } => f
                .debug_struct("Openai")
                .field("base_url", base_url)
                .finish_non_exhaustive(),
        }
    }
}

/// Request budgets. Windows are fixed in the handlers: sign-in 10 minutes, mail 1 hour,
/// token endpoint 1 minute, realtime tokens 1 hour.
#[derive(Debug, Clone)]
pub struct Limits {
    pub signin_per_ip: u32,
    pub signin_per_email: u32,
    pub mail_per_email: u32,
    pub mail_per_ip: u32,
    pub token_per_ip: u32,
    pub realtime_per_account: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            signin_per_ip: 20,
            signin_per_email: 10,
            mail_per_email: 5,
            mail_per_ip: 20,
            token_per_ip: 60,
            realtime_per_account: 20,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    /// No trailing slash. Used for links in emails and to decide the `Secure` cookie flag.
    pub public_base_url: String,
    pub mail_from: String,
    pub realtime: RealtimeConfig,
    pub limits: Limits,
}

/// Values read from the environment that are not part of [`Config`] because they are only
/// needed to build the pool and the mailer.
#[derive(Clone)]
pub struct Env {
    pub config: Config,
    pub database_url: String,
    pub smtp_url: String,
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Env {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let need = |k: &'static str| {
            get(k)
                .filter(|v| !v.is_empty())
                .ok_or(ConfigError::Missing(k))
        };
        let database_url = need("DATABASE_URL")?;
        let bind_addr = need("BIND_ADDR")?
            .parse()
            .map_err(|_| ConfigError::Invalid("BIND_ADDR"))?;
        let public_base_url = need("PUBLIC_BASE_URL")?.trim_end_matches('/').to_owned();
        if !public_base_url.starts_with("http://") && !public_base_url.starts_with("https://") {
            return Err(ConfigError::Invalid("PUBLIC_BASE_URL"));
        }
        let smtp_url = need("SMTP_URL")?;
        let mail_from = need("MAIL_FROM")?;
        let realtime = match need("REALTIME_PROVIDER")?.as_str() {
            "fake" => RealtimeConfig::Fake,
            "openai" => RealtimeConfig::Openai {
                api_key: need("OPENAI_API_KEY")?,
                base_url: get("OPENAI_BASE_URL")
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| "https://api.openai.com".to_owned()),
                transcribe_model: get("OPENAI_TRANSCRIBE_MODEL")
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| "gpt-live-transcribe".to_owned()),
            },
            _ => return Err(ConfigError::Invalid("REALTIME_PROVIDER")),
        };
        Ok(Self {
            config: Config {
                bind_addr,
                public_base_url,
                mail_from,
                realtime,
                limits: Limits::default(),
            },
            database_url,
            smtp_url,
        })
    }
}
