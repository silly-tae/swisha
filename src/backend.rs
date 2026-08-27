//! Opening the PostgreSQL connection pool and the notifier.

// Assembles the storage and notification pair for whichever backend is compiled in. Exactly
// one is enabled at a time, so `state::Store` and `state::Notifications` stay concrete types.

use crate::config::Config;
use crate::error::{Context, Result};
use crate::state::{Notifications, Store};

/// Opens the connection pool and the notifier for the compiled-in backend.
pub async fn connect(config: &Config) -> Result<(Store, Notifications)> {
    use std::time::Duration;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let options = if config.db_host.starts_with('/') {
        PgConnectOptions::new().socket(&config.db_host)
    } else {
        let (host, port) = match config.db_host.rsplit_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().unwrap_or(5432)),
            None => (config.db_host.as_str(), 5432),
        };
        PgConnectOptions::new().host(host).port(port)
    }
    .username(&config.db_user)
    .password(&config.db_pass)
    .database(&config.db_name);

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(5))
        .max_lifetime(Duration::from_secs(1800))
        .test_before_acquire(false)
        .idle_timeout(Duration::from_secs(300))
        // Cap query execution so a runaway statement cannot hold a connection indefinitely.
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout = '10s'")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .context("Cannot connect to PostgreSQL")?;

    let store = Store::new(
        pool.clone(),
        &config.table_payouts,
        &config.table_events,
        &config.table_logs,
    );
    Ok((store, Notifications::new(pool)))
}
