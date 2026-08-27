#![forbid(unsafe_code)]
use std::{net::SocketAddr, sync::Arc, time::SystemTime};
use tokio::net::TcpListener;
use tracing::Level;
use swisha::{backend, config, events, http, state, store::PayoutStore, swish};

pub const SCHEMA: &str = include_str!("../schema/postgres.sql");

// A payout that has not moved in this long is treated as stalled, and swisha asks Swish what
// became of it. It is never resubmitted.
const STALL_AFTER: std::time::Duration = std::time::Duration::from_secs(30 * 60);

#[tokio::main]
async fn main() {
    // Answered before anything else so it works without a database or certificates.
    if std::env::args().any(|a| a == "--print-schema") {
        print!("{SCHEMA}");
        return;
    }

    // Read the configuration file, if one was named, before anything reads a setting.
    let env = swisha::env::Env::discover().unwrap_or_else(|e| {
        eprintln!("Configuration error: {e}");
        std::process::exit(1);
    });

    tracing_subscriber::fmt().with_max_level(log_level(&env)).init();
    if let Some(path) = env.source() {
        tracing::info!("configuration loaded from {}", path.display());
    }

    let config = config::Config::from_env(&env).unwrap_or_else(|e| {
        eprintln!("Configuration error: {e}");
        std::process::exit(1);
    });

    let swish_client = swish::client::build_swish_client(&config).unwrap_or_else(|e| {
        eprintln!("Swish client error: {e}");
        std::process::exit(1);
    });

    let signing_key = {
        Arc::new(swish::sign::rsa_key_pair(&config.swish_signing_key).unwrap_or_else(|e| {
            eprintln!("Swish signing key error: {e}");
            std::process::exit(1);
        }))
    };

    let started_at = SystemTime::now();

    let (store, notifier) = backend::connect(&config).await.unwrap_or_else(|e| {
        eprintln!("Storage error: {e}");
        std::process::exit(1);
    });

    let state: state::SharedState = Arc::new(state::AppState {
        store,
        notifier,
        stream: events::EventStream::default(),
        config: config.clone(),
        swish_client,
        signing_key,
        started_at,
        swish_probe: tokio::sync::RwLock::new(None),
    });

    // Swish reachability, refreshed every 30s. Only this half is cached: reaching Swish is slow
    // and they rate-limit, so a probe polling health must not turn into a call to them each time.
    //
    // The first tick fires immediately and is used, not discarded. An empty cache used to mean
    // every health check reached out to Swish itself, which is a burst at them on every restart.
    {
        let s = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                let probe = http::routes::health::probe_swish(&s).await;
                *s.swish_probe.write().await = Some(probe);
            }
        });
    }

    // Sweep payouts that have not moved in 30 minutes. CREATED means the submit or poll died
    // mid-flight, PENDING means Swish never resolved it, ERROR means a failed poll left it
    // stranded; none of them recover on their own.
    {
        let s = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(300));
            interval.tick().await;
            loop {
                interval.tick().await;
                let stalled = match s
                    .store
                    .claim_stalled(swish::reconcile::MAX_ATTEMPTS, STALL_AFTER)
                    .await
                {
                    Ok(payouts) => payouts,
                    Err(e) => {
                        tracing::warn!("[stuck-sweep] store error: {e}");
                        continue;
                    }
                };
                for payout in stalled {
                    tracing::warn!("[stuck-sweep] payout stalled for 30 minutes: {}", payout.reference);
                    let s2 = s.clone();
                    tokio::spawn(async move {
                        swish::reconcile::reconcile(payout.reference, payout.attempts, s2).await;
                    });
                }
            }
        });
    }

    events::log_admin_event("info", "service_started", serde_json::json!({}), "", &state).await;

    let internal = http::internal_router().with_state(state.clone());
    let callback = http::callback_router().with_state(state);

    let callback_listener = bind(&config.callback_addr).await;

    tracing::info!(
        "swisha payout and health listening on {}",
        config.internal.describe()
    );
    if config.api_secret.is_none() {
        tracing::info!("no shared secret configured: the listener itself is the boundary");
    }
    tracing::info!("swisha callback listening on {}", config.callback_addr);
    if config.swish_env == "production" {
        let ips = http::routes::callback::active_callback_ips();
        tracing::info!("callback IP whitelist ({} entries): {}", ips.len(), ips.join(", "));
    } else {
        tracing::warn!("swish running in '{}' mode – callback IP allowlist is DISABLED, any IP can submit callbacks", config.swish_env);
    }

    let callback_service = callback.into_make_service_with_connect_info::<SocketAddr>();

    // Either listener going down takes the process with it, so a supervisor restarts both
    // rather than leaving the service half-serving.
    match &config.internal {
        config::InternalListener::Tcp(addr) => {
            let listener = bind(addr).await;
            let service = internal.into_make_service_with_connect_info::<SocketAddr>();
            tokio::select! {
                result = async { axum::serve(listener, service).await } => {
                    eprintln!("Internal server stopped: {result:?}");
                }
                result = async { axum::serve(callback_listener, callback_service).await } => {
                    eprintln!("Callback server stopped: {result:?}");
                }
            }
        }
        config::InternalListener::Unix(path) => {
            let listener = bind_socket(path);
            let service = internal.into_make_service();
            tokio::select! {
                result = async { axum::serve(listener, service).await } => {
                    eprintln!("Internal server stopped: {result:?}");
                }
                result = async { axum::serve(callback_listener, callback_service).await } => {
                    eprintln!("Callback server stopped: {result:?}");
                }
            }
        }
    }
    std::process::exit(1);
}

// The permission bits are the authentication, so they are set here rather than inherited from
// the umask: 022 would leave the socket unwritable by its own group, 000 would open it to the
// whole host.
fn bind_socket(path: &std::path::Path) -> tokio::net::UnixListener {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let fail = |message: String| -> ! {
        eprintln!("{message}");
        std::process::exit(1);
    };

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        fail(format!("Socket directory does not exist: {}", parent.display()));
    }

    // An unclean shutdown leaves the entry behind, and bind refuses to replace it. Removing a
    // stale socket is safe; removing anything else is not.
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            if let Err(e) = std::fs::remove_file(path) {
                fail(format!("Cannot remove stale socket {}: {e}", path.display()));
            }
        }
        Ok(_) => fail(format!(
            "{} exists and is not a socket, refusing to replace it",
            path.display()
        )),
        Err(_) => {}
    }

    let listener = tokio::net::UnixListener::bind(path)
        .unwrap_or_else(|e| fail(format!("Failed to bind {}: {e}", path.display())));

    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660)) {
        fail(format!("Cannot set permissions on {}: {e}", path.display()));
    }

    // Confirm rather than assume: this is the whole access control.
    match std::fs::metadata(path) {
        Ok(meta) if meta.permissions().mode() & 0o007 != 0 => fail(format!(
            "{} is world-accessible ({:o}), refusing to serve payouts",
            path.display(),
            meta.permissions().mode() & 0o777
        )),
        Ok(_) => {}
        Err(e) => fail(format!("Cannot read back {}: {e}", path.display())),
    }

    listener
}

async fn bind(addr: &str) -> TcpListener {
    TcpListener::bind(addr).await.unwrap_or_else(|e| {
        eprintln!("Failed to bind to {addr}: {e}");
        std::process::exit(1);
    })
}

// RUST_LOG takes a bare level here. Per-target directives such as "swisha=debug,sqlx=warn"
// need tracing-subscriber's env-filter feature, which pulls in three regex crates.
fn log_level(env: &swisha::env::Env) -> Level {
    match env.optional("RUST_LOG", "").trim().to_ascii_lowercase().as_str() {
        "error" => Level::ERROR,
        "warn"  => Level::WARN,
        "debug" => Level::DEBUG,
        "trace" => Level::TRACE,
        _       => Level::INFO,
    }
}

