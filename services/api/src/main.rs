use std::{sync::Arc, time::Duration};

use sotto_api::{AppState, MIGRATOR, config::Env, mail::SmtpMailer, purge_expired, router};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error(transparent)]
    Config(#[from] sotto_api::config::ConfigError),
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migrations: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("mail transport is misconfigured")]
    Mail,
    #[error("state: {0}")]
    State(#[from] sotto_api::error::ApiError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn")),
        )
        .init();
    if let Err(e) = run().await {
        tracing::error!(error = %e, "startup failed");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), StartupError> {
    let env = Env::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&env.database_url)
        .await?;
    MIGRATOR.run(&pool).await?;
    let mailer =
        SmtpMailer::new(&env.smtp_url, &env.config.mail_from).map_err(|_| StartupError::Mail)?;
    let addr = env.config.bind_addr;
    let state = AppState::new(pool.clone(), env.config, Arc::new(mailer))?;

    tokio::spawn(async move {
        loop {
            if let Err(e) = purge_expired(&pool).await {
                tracing::warn!(error = %e, "purge failed");
            }
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
