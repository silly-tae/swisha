//! PostgreSQL backend.

use std::time::Duration;

use sqlx::PgPool;

use crate::domain::payout::{
    ClaimOutcome, EventRecord, NewPayout, PayoutSnapshot, StalledPayout, StatusView,
};
use crate::domain::status;
use crate::error::Result;
use crate::store::PayoutStore;

/// PostgreSQL storage.
///
/// The atomic claim is one `INSERT ... ON CONFLICT DO UPDATE ... RETURNING`, which is what makes
/// concurrent submissions of one reference produce exactly one payout.
pub struct PostgresStore {
    pool: PgPool,
    payouts: String,
    events: String,
    logs: String,
    // Built once. The table names and the status set are fixed at startup, so rendering
    // this on every payout was work with a known answer.
    claim_sql: String,
}

impl PostgresStore {
    // Table names are interpolated into statements, so they must already be validated as plain
    // identifiers. Config does that before construction. Values are always bound.
    /// Wraps a pool and the three table names, which are validated as plain identifiers
    /// before they reach here.
    pub fn new(pool: PgPool, payouts: &str, events: &str, logs: &str) -> Self {
        let locked = format!(
            "{}.status IN ({})",
            payouts,
            status::sql_list(status::FIELDS_LOCKED)
        );
        // ON CONFLICT DO UPDATE gives the atomicity the contract requires, and RETURNING
        // reports the resulting row in the same round trip.
        let claim_sql = format!(
            "INSERT INTO {tbl}
                (reference, payee_alias, payee_ssn, amount, message, swish_ref, status, attempts)
             VALUES ($1, $2, $3, $4, $5, $6, 'CREATED', 0)
             ON CONFLICT (reference) DO UPDATE SET
                payee_alias = CASE WHEN {locked} THEN {tbl}.payee_alias ELSE EXCLUDED.payee_alias END,
                payee_ssn   = CASE WHEN {locked} THEN {tbl}.payee_ssn   ELSE EXCLUDED.payee_ssn   END,
                amount      = CASE WHEN {locked} THEN {tbl}.amount      ELSE EXCLUDED.amount      END,
                message     = CASE WHEN {locked} THEN {tbl}.message     ELSE EXCLUDED.message     END,
                swish_ref   = CASE WHEN {locked} THEN {tbl}.swish_ref   ELSE EXCLUDED.swish_ref   END,
                status      = CASE WHEN {locked} THEN {tbl}.status      ELSE 'CREATED'            END,
                attempts    = CASE WHEN {locked} THEN {tbl}.attempts    ELSE 0                    END,
                updated_at  = CASE WHEN {locked} THEN {tbl}.updated_at  ELSE NOW()                END
             RETURNING swish_ref, status",
            tbl = payouts
        );
        Self {
            pool,
            payouts: payouts.to_string(),
            events: events.to_string(),
            logs: logs.to_string(),
            claim_sql,
        }
    }

    /// The underlying pool, so a caller can share one connection pool with its own queries.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl PayoutStore for PostgresStore {
    async fn claim(&self, new: &NewPayout<'_>) -> Result<ClaimOutcome> {
        let (swish_ref, status): (Option<String>, String) = sqlx::query_as(&self.claim_sql)
            .bind(new.reference)
            .bind(new.payee_alias)
            .bind(new.payee_ssn)
            .bind(new.amount)
            .bind(new.message)
            .bind(new.swish_ref)
            .fetch_one(&self.pool)
            .await?;

        Ok(ClaimOutcome { status, swish_ref })
    }

    async fn set_status_unless_terminal(&self, reference: &str, status: &str) -> Result<bool> {
        let result = sqlx::query(&format!(
            "UPDATE {} SET status = $1, updated_at = NOW() WHERE reference = $2 AND {}",
            self.payouts,
            status::writable_condition("$1")
        ))
        .bind(status)
        .bind(reference)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn snapshot(&self, reference: &str) -> Result<Option<PayoutSnapshot>> {
        let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(&format!(
            "SELECT status, swish_ref FROM {} WHERE reference = $1 LIMIT 1",
            self.payouts
        ))
        .bind(reference)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(status, swish_ref)| PayoutSnapshot { status, swish_ref }))
    }

    async fn status_view(&self, reference: &str) -> Result<Option<StatusView>> {
        type Row = (Option<String>, Option<String>, Option<String>);
        let row: Option<Row> = sqlx::query_as(&format!(
            "SELECT p.status, p.swish_ref,
                    (SELECT e.error_code FROM {events} e
                      WHERE e.reference = p.reference AND e.error_code IS NOT NULL
                      ORDER BY e.id DESC LIMIT 1)
               FROM {payouts} p WHERE p.reference = $1 LIMIT 1",
            events = self.events,
            payouts = self.payouts
        ))
        .bind(reference)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(status, swish_ref, error_code)| StatusView { status, swish_ref, error_code }))
    }

    async fn claim_stalled(&self, max_attempts: i32, older_than: Duration) -> Result<Vec<StalledPayout>> {
        let rows: Vec<(String, i32)> = sqlx::query_as(&format!(
            "UPDATE {} SET attempts = attempts + 1, updated_at = NOW()
             WHERE status IN ({})
             AND attempts < $1
             AND updated_at < NOW() - make_interval(secs => $2)
             RETURNING reference, attempts",
            self.payouts,
            status::sql_list(status::STALLED)
        ))
        .bind(max_attempts)
        .bind(older_than.as_secs_f64())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(reference, attempts)| StalledPayout { reference, attempts })
            .collect())
    }

    async fn latest_error_code(&self, reference: &str) -> Result<Option<String>> {
        let code: Option<Option<String>> = sqlx::query_scalar(&format!(
            "SELECT error_code FROM {} WHERE reference = $1 AND error_code IS NOT NULL
             ORDER BY id DESC LIMIT 1",
            self.events
        ))
        .bind(reference)
        .fetch_optional(&self.pool)
        .await?;
        Ok(code.flatten())
    }

    async fn record_event(&self, event: &EventRecord) -> Result<()> {
        sqlx::query(&format!(
            "INSERT INTO {}
             (reference, swish_ref, event, status, amount, payee_alias, error_code, error_message, ip)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            self.events
        ))
        .bind(&event.reference)
        .bind(&event.swish_ref)
        .bind(&event.event)
        .bind(&event.status)
        .bind(event.amount)
        .bind(&event.payee_alias)
        .bind(&event.error_code)
        .bind(&event.error_message)
        .bind(&event.ip)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn record_log(
        &self,
        level: &str,
        message: &str,
        context: Option<&str>,
        ip: Option<&str>,
    ) -> Result<()> {
        sqlx::query(&format!(
            "INSERT INTO {} (level, message, context, ip) VALUES ($1, $2, $3, $4)",
            self.logs
        ))
        .bind(level)
        .bind(message)
        .bind(context)
        .bind(ip)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}
