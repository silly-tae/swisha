// Shared test harness. Each test binary compiles the whole module and uses a slice of it, so
// unused items here are expected rather than dead.
#![allow(dead_code)]

use std::sync::{Arc, OnceLock};

use ring::signature::RsaKeyPair;
use swisha::config::{Config, InternalListener};
use swisha::domain::errors::Language;
use swisha::state::{AppState, Notifications, SharedState, Store};

pub const SECRET: &str = "0123456789abcdef0123456789abcdef";
pub const SSN: &str = "196408233234";
pub const PHONE: &str = "0701234567";

// Generated per run and never written into the repository: a private key committed to an
// open-source payments project is exactly what secret scanning exists to stop.
pub fn signing_key() -> Arc<RsaKeyPair> {
    static KEY: OnceLock<Arc<RsaKeyPair>> = OnceLock::new();
    KEY.get_or_init(|| {
        let out = std::process::Command::new("openssl")
            .args(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048"])
            .output()
            .expect("openssl is required to generate a throwaway signing key");
        assert!(out.status.success(), "openssl genpkey failed");
        let pem = String::from_utf8(out.stdout).expect("PEM is UTF-8");
        let der = swisha::swish::sign::pem_to_der(&pem).expect("decode the generated PEM");
        Arc::new(RsaKeyPair::from_pkcs8(&der).expect("parse the generated RSA key"))
    })
    .clone()
}

// Where the test database lives. Unset is a hard stop rather than a skip: a payments suite
// reporting "0 failed" while its double-payout guards never ran is the worst outcome available.
pub fn database_url() -> String {
    std::env::var("SWISHA_TEST_DATABASE_URL").unwrap_or_else(|_| {
        panic!(
            "\n\nSWISHA_TEST_DATABASE_URL is not set, and every test needs a database.\n\n  \
             docker run -d --name swisha-pg -p 5433:5432 \\\n    \
             -e POSTGRES_USER=swisha -e POSTGRES_PASSWORD=swisha \\\n    \
             -e POSTGRES_DB=swisha_test postgres:16\n\n  \
             export SWISHA_TEST_DATABASE_URL=postgres://swisha:swisha@localhost:5433/swisha_test\n"
        )
    })
}

// One prefix per call, so tests sharing a server cannot see each other's rows. This is the same
// TABLE_* namespacing a real multi-instance deployment uses, not a test-only mechanism.
pub fn table_prefix() -> String {
    format!("t{}", swisha::swish::random_payout_uuid()[..12].to_lowercase())
}

pub fn config() -> Config {
    let prefix = table_prefix();
    Config {
        internal:             InternalListener::Tcp("127.0.0.1:0".into()),
        callback_addr:        "127.0.0.1:0".into(),
        db_host:              "127.0.0.1".into(),
        db_name:              "swisha_test".into(),
        db_user:              "swisha".into(),
        db_pass:              String::new(),
        table_payouts:        format!("{prefix}_payouts"),
        table_logs:           format!("{prefix}_logs"),
        table_events:         format!("{prefix}_events"),
        trusted_proxies:      Vec::new(),
        api_secret:           Some(SECRET.into()),
        swish_env:            "test".into(),
        swish_base_url:       "http://127.0.0.1:1".into(),
        swish_number:         "1234679304".into(),
        swish_max_payout:     50_000.0,
        swish_callback_url:   "https://example.test/swish/callback".into(),
        swish_tls_cert:       String::new(),
        swish_tls_key:        String::new(),
        swish_ca:             None,
        swish_signing_key:    String::new(),
        swish_signing_serial: "00".into(),
        payout_message:       "{reference}".into(),
        notify_prefix:        "swisha".into(),
        error_language:       Language::English,
        require_ssn:          false,
    }
}

// Applies the shipped schema under this config's table prefix. `swisha_` is the only name in
// schema/postgres.sql, so substituting it is the whole of the isolation.
pub async fn pool_with_schema(config: &Config) -> sqlx::PgPool {
    let prefix = config
        .table_payouts
        .strip_suffix("_payouts")
        .expect("config() builds table names from a prefix");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&database_url())
        .await
        .unwrap_or_else(|e| panic!("connect to {}: {e}", database_url()));

    sweep_old_tables(&pool).await;

    let ddl = include_str!("../../schema/postgres.sql")
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("swisha_", &format!("{prefix}_"));

    for statement in ddl.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("schema statement failed: {e}\n{statement}"));
    }
    pool
}

// Tests do not drop their own tables, because a panicking test would leave them behind anyway.
// Instead the first pool of each run clears what earlier runs left, which is self-healing and
// needs no guard threaded through 300 call sites.
async fn sweep_old_tables(pool: &sqlx::PgPool) {
    static SWEPT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    SWEPT
        .get_or_init(|| async {
            let names: Vec<String> = sqlx::query_scalar(
                "SELECT tablename FROM pg_tables WHERE schemaname = current_schema() \
                 AND tablename ~ '^t[0-9a-f]{12}_(payouts|events|logs)$'",
            )
            .fetch_all(pool)
            .await
            .unwrap_or_default();

            for name in names {
                let _ = sqlx::query(&format!("DROP TABLE IF EXISTS {name} CASCADE"))
                    .execute(pool)
                    .await;
            }
        })
        .await;
}

// Hands the pool back alongside the state, so a test can count rows without the store needing a
// public accessor it would not otherwise have.
pub async fn state_and_pool(config: Config) -> (SharedState, sqlx::PgPool) {
    let pool = pool_with_schema(&config).await;
    let state = state_over(pool.clone(), config);
    (state, pool)
}

fn state_over(pool: sqlx::PgPool, config: Config) -> SharedState {
    Arc::new(AppState {
        store: Store::new(pool.clone(), &config.table_payouts, &config.table_events, &config.table_logs),
        notifier: Notifications::new(pool),
        stream: swisha::events::EventStream::default(),
        config,
        swish_client: reqwest::Client::new(),
        signing_key: signing_key(),
        started_at: std::time::SystemTime::now(),
        swish_probe: tokio::sync::RwLock::new(None),
    })
}

pub async fn state_with(config: Config) -> SharedState {
    let pool = pool_with_schema(&config).await;
    state_over(pool, config)
}

pub async fn state() -> SharedState {
    state_with(config()).await
}

// Mirrors main.rs, including the connect-info service the callback handler needs.
#[cfg(feature = "http")]
pub async fn serve(router: axum::Router) -> String {
    use std::net::SocketAddr;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .ok();
    });
    format!("http://{addr}")
}

// A stand-in for Swish's Payouts API, so the submit and poll paths can be driven through every
// answer Swish documents without touching MSS. Scripted, and it records what it was sent.
#[cfg(feature = "http")]
#[derive(Clone, Default)]
pub struct MockSwish {
    inner: Arc<std::sync::Mutex<MockInner>>,
}

#[cfg(feature = "http")]
#[derive(Default)]
struct MockInner {
    post_status: Option<u16>,
    post_body: String,
    get_status: Option<String>,
    posts: Vec<serde_json::Value>,
    gets: usize,
}

#[cfg(feature = "http")]
impl MockSwish {
    pub fn new() -> Self {
        Self::default()
    }

    // Swish answers 201 with a Location header when it accepts an instruction.
    pub fn accepts(self) -> Self {
        self.inner.lock().unwrap().post_status = Some(201);
        self
    }

    pub fn rejects(self, status: u16, body: &str) -> Self {
        let mut i = self.inner.lock().unwrap();
        i.post_status = Some(status);
        i.post_body = body.to_string();
        drop(i);
        self
    }

    // What a GET on the instruction returns. None answers 404, which Swish uses for a UUID it
    // has no record of.
    pub fn resolves_to(self, status: &str) -> Self {
        self.inner.lock().unwrap().get_status = Some(status.to_string());
        self
    }

    pub fn posted(&self) -> Vec<serde_json::Value> {
        self.inner.lock().unwrap().posts.clone()
    }

    pub fn get_count(&self) -> usize {
        self.inner.lock().unwrap().gets
    }

    pub async fn start(self) -> String {
        use axum::{extract::State, routing::{get, post}, Json, Router};
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        async fn create(
            State(m): State<MockSwish>,
            Json(body): Json<serde_json::Value>,
        ) -> impl IntoResponse {
            let mut i = m.inner.lock().unwrap();
            i.posts.push(body);
            match i.post_status {
                Some(201) | None => (StatusCode::CREATED, String::new()),
                Some(code) => (
                    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    i.post_body.clone(),
                ),
            }
        }

        async fn read(State(m): State<MockSwish>) -> impl IntoResponse {
            let mut i = m.inner.lock().unwrap();
            i.gets += 1;
            match &i.get_status {
                Some(s) => (StatusCode::OK, Json(serde_json::json!({ "status": s }))).into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }

        let router = Router::new()
            .route("/swish-cpcapi/api/v1/payouts", post(create))
            .route("/swish-cpcapi/api/v1/payouts/{uuid}", get(read))
            .with_state(self);
        serve(router).await
    }
}
