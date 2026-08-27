#![cfg(feature = "http")]

// One business, many locations, one database. Each location runs its own swisha with its own
// Swish number, its own tables and its own notify prefix. Nothing may leak between them: not a
// payout, not a reference, not a live event, and above all not a merchant number.

mod common;

use common::{MockSwish, PHONE, SECRET, SSN};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};
use swisha::state::SharedState;
use swisha::store::PayoutStore;

static NEXT: AtomicU32 = AtomicU32::new(0);

fn caller() -> String {
    format!("198.51.100.{}", NEXT.fetch_add(1, Ordering::Relaxed) % 254 + 1)
}

// Eight, so nothing here can quietly depend on there being two or three. Each is a separate
// swisha process with its own Swish number, tables and notify prefix, sharing one database.
const CITIES: [(&str, &str); 8] = [
    ("malmo", "1231111111"),
    ("helsingborg", "1232222222"),
    ("landskrona", "1233333333"),
    ("lund", "1234444444"),
    ("ystad", "1235555555"),
    ("trelleborg", "1236666666"),
    ("kristianstad", "1237777777"),
    ("hassleholm", "1238888888"),
];

struct City {
    name: &'static str,
    number: &'static str,
    base: String,
    state: SharedState,
}

// One database for all of them, the way a single VPS would run it. Each city's tables are the
// shipped schema with its own prefix, which is exactly what a real deployment does.
async fn deploy(swish_url: &str) -> (Vec<City>, sqlx::PgPool, String) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&common::database_url())
        .await
        .expect("open shared database");

    // One run's cities share a prefix so a repeated run cannot collide with the last one.
    let run = common::table_prefix();
    let template: String = include_str!("../schema/postgres.sql")
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut cities = Vec::new();
    for (name, number) in CITIES {
        let ddl = template.replace("swisha_", &format!("{run}_{name}_"));
        for statement in ddl.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("{name}: {e}\n{statement}"));
        }

        let mut config = common::config();
        config.swish_base_url = swish_url.to_string();
        config.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
        config.swish_number = number.to_string();
        config.table_payouts = format!("{run}_{name}_payouts");
        config.table_events = format!("{run}_{name}_events");
        config.table_logs = format!("{run}_{name}_logs");
        config.notify_prefix = format!("{run}_{name}");

        let state = std::sync::Arc::new(swisha::state::AppState {
            store: swisha::state::Store::new(
                pool.clone(),
                &config.table_payouts,
                &config.table_events,
                &config.table_logs,
            ),
            notifier: swisha::state::Notifications::new(pool.clone()),
            stream: swisha::events::EventStream::default(),
            config,
            swish_client: reqwest::Client::new(),
            signing_key: common::signing_key(),
            started_at: std::time::SystemTime::now(),
            swish_probe: Default::default(),
        });
        let base = common::serve(swisha::http::internal_router().with_state(state.clone())).await;
        cities.push(City { name, number, base, state });
    }
    (cities, pool, run)
}

async fn pay(city: &City, reference: &str, amount: f64) -> u16 {
    reqwest::Client::new()
        .post(format!("{}/swish/payout", city.base))
        .header("x-api-secret", SECRET)
        .header("x-forwarded-for", caller())
        .json(&json!({
            "reference": reference,
            "payee_alias": PHONE,
            "payee_ssn": SSN,
            "amount": amount,
        }))
        .send()
        .await
        .expect("request")
        .status()
        .as_u16()
}

fn payload_of(sent: &Value) -> Value {
    match sent["payload"].as_str() {
        Some(raw) => serde_json::from_str(raw).expect("json"),
        None => sent["payload"].clone(),
    }
}

// The one that matters: a payout must be sent under its own location's Swish number, never a
// neighbour's, or the money leaves the wrong account.
#[tokio::test]
async fn each_location_pays_from_its_own_swish_number() {
    let mock = MockSwish::new().accepts().resolves_to("PAID");
    let (cities, _pool, _run) = deploy(&mock.clone().start().await).await;

    for city in &cities {
        assert_eq!(pay(city, &format!("{}-1", city.name), 100.0).await, 202, "{}", city.name);
    }

    let sent = mock.posted();
    assert_eq!(sent.len(), CITIES.len(), "one instruction per location");

    for instruction in &sent {
        let payload = payload_of(instruction);
        let reference = payload["payerPaymentReference"].as_str().expect("reference");
        let city = cities
            .iter()
            .find(|c| reference.starts_with(c.name))
            .unwrap_or_else(|| panic!("unknown reference {reference}"));
        assert_eq!(
            payload["payerAlias"], city.number,
            "{} paid from the wrong Swish number", city.name
        );
    }
}

// Two locations invoicing the same number is ordinary, and their tables are separate, so it must
// simply be two payouts rather than a collision.
#[tokio::test]
async fn the_same_reference_in_two_locations_is_two_payouts() {
    let mock = MockSwish::new().accepts().resolves_to("PAID");
    let (cities, _pool, _run) = deploy(&mock.clone().start().await).await;

    for city in &cities {
        assert_eq!(pay(city, "INV-1042", 100.0).await, 202, "{}", city.name);
    }
    assert_eq!(mock.posted().len(), CITIES.len(), "every location's payout went out");

    // And each one is refused a second time, within its own location.
    for city in &cities {
        assert_eq!(pay(city, "INV-1042", 100.0).await, 409, "{}", city.name);
    }
    assert_eq!(mock.posted().len(), CITIES.len(), "no location sent a second instruction");
}

#[tokio::test]
async fn a_location_only_sees_its_own_payouts() {
    let mock = MockSwish::new().accepts().resolves_to("PAID");
    let (cities, pool, run) = deploy(&mock.clone().start().await).await;

    for city in &cities {
        pay(city, &format!("{}-A", city.name), 50.0).await;
    }

    for city in &cities {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {run}_{}_payouts", city.name))
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(rows, 1, "{} should hold exactly its own payout", city.name);

        // Its own reference is there, and a neighbour's is not.
        assert!(city.state.store.snapshot(&format!("{}-A", city.name)).await.expect("store").is_some());
        for other in cities.iter().filter(|o| o.name != city.name) {
            assert!(
                city.state.store.snapshot(&format!("{}-A", other.name)).await.expect("store").is_none(),
                "{} can see {}'s payout", city.name, other.name
            );
        }
    }
}

// The notify prefix is the setting a deployment is most likely to leave at its default, and the
// symptom would be one location's screen showing another's payouts.
#[tokio::test]
async fn live_events_do_not_cross_between_locations() {
    let mock = MockSwish::new().accepts().resolves_to("PAID");
    let (cities, _pool, _run) = deploy(&mock.clone().start().await).await;

    let mut streams: Vec<_> = cities
        .iter()
        .map(|c| (c.name, c.state.config.notify_prefix.clone(), c.state.stream.subscribe()))
        .collect();
    for city in &cities {
        pay(city, &format!("{}-EV", city.name), 25.0).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    for (name, prefix, rx) in &mut streams {
        let mut seen = 0;
        while let Ok(event) = rx.try_recv() {
            assert!(
                event.channel.starts_with(&format!("{prefix}:")),
                "{name} received an event on channel {}", event.channel
            );
            if let Some(reference) = &event.reference {
                assert!(reference.starts_with(*name), "{name} received {reference}");
            }
            seen += 1;
        }
        assert!(seen > 0, "{name} saw nothing on its own stream");
    }
}

// All of them at once, on one database, which is the arrangement that would actually run.
#[tokio::test]
async fn every_location_can_pay_at_the_same_time() {
    let mock = MockSwish::new().accepts().resolves_to("PAID");
    let (cities, _pool, _run) = deploy(&mock.clone().start().await).await;

    let mut tasks = Vec::new();
    for city in &cities {
        for n in 0..4 {
            let (base, name) = (city.base.clone(), city.name);
            tasks.push(tokio::spawn(async move {
                reqwest::Client::new()
                    .post(format!("{base}/swish/payout"))
                    .header("x-api-secret", SECRET)
                    .header("x-forwarded-for", caller())
                    .json(&json!({
                        "reference": format!("{name}-C{n}"),
                        "payee_alias": PHONE,
                        "payee_ssn": SSN,
                        "amount": 10.0,
                    }))
                    .send()
                    .await
                    .expect("request")
                    .status()
                    .as_u16()
            }));
        }
    }

    let mut accepted = 0;
    for t in tasks {
        if t.await.expect("join") == 202 {
            accepted += 1;
        }
    }
    let expected = CITIES.len() * 4;
    assert_eq!(accepted, expected, "every location's payouts should be accepted");
    assert_eq!(mock.posted().len(), expected, "and each sent exactly one instruction");
}
