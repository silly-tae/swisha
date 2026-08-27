//! PostgreSQL `LISTEN`/`NOTIFY`.

use sqlx::PgPool;

use crate::error::Result;
use crate::notify::Notifier;

// LISTEN/NOTIFY. Only reaches sessions that are listening at the moment of the call.
/// Publishes through PostgreSQL `NOTIFY`, so listeners in other processes hear updates.
pub struct PostgresNotifier {
    pool: PgPool,
}

impl PostgresNotifier {
    /// Wraps a connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl Notifier for PostgresNotifier {
    async fn publish(&self, channel: &str, payload: &str) -> Result<()> {
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(channel)
            .bind(payload)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
