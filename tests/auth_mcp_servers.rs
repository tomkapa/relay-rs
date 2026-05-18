//! End-to-end probe for the tenancy foundation.
//!
//! Verifies the three contracts the task exit criterion calls out:
//!   1. Unauthenticated `GET /mcp-servers` → 401.
//!   2. Authenticated `GET /mcp-servers` for a new org → 200, `[]`.
//!   3. Cross-org isolation: a row inserted under org A is invisible to
//!      a request authenticated as org B.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use relay_rs::auth::OrgId;
use relay_rs::clock::SystemClock;
use relay_rs::http::{AppState, router};
use relay_rs::mcp::{
    McpHttpUrl, McpRefresher, McpRegistry, McpServerAlias, McpServerCreate, McpTransport,
    PgMcpServerStore, SharedMcpServerStore,
};
use relay_rs::runtime::{
    PgDagBudget, PgPromptQueue, PgResponseHub, PgThreadStream, SharedDagBudget, SharedLeaseManager,
    SharedPromptQueue, SharedResponseSink, SharedResponseSource, SharedThreadStream,
};
use relay_rs::session::{PgSessionStore, SharedSessionStore};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

mod common;
use common::auth::{SeededPrincipal, seed_principal};
use common::pg::TestDb;

struct AuthMcpHarness {
    state: AppState,
    mcp_store: SharedMcpServerStore,
    primary: SeededPrincipal,
    #[allow(dead_code)]
    refresher: McpRefresher,
    #[allow(dead_code)]
    db: TestDb,
}

impl AuthMcpHarness {
    async fn new() -> Self {
        let db = TestDb::fresh().await;
        let clock = SystemClock::shared();
        let pool: PgPool = db.pool.clone();

        let queue_impl = Arc::new(PgPromptQueue::new(pool.clone(), clock.clone()));
        let queue: SharedPromptQueue = queue_impl.clone();
        let leases: SharedLeaseManager = queue_impl;

        let hub = Arc::new(PgResponseHub::new(pool.clone(), clock.clone()));
        let _sink: SharedResponseSink = hub.clone();
        let responses: SharedResponseSource = hub;

        let sessions: SharedSessionStore =
            Arc::new(PgSessionStore::new(pool.clone(), clock.clone()));
        let agents = common::pg::shared_agent_store(pool.clone(), clock.clone());
        let dag: SharedDagBudget = Arc::new(PgDagBudget::new(pool.clone()));

        let mcp_store: SharedMcpServerStore =
            Arc::new(PgMcpServerStore::new(pool.clone(), clock.clone()));
        let mcp_registry = McpRegistry::new(mcp_store.clone(), clock.clone());
        let (refresher, mcp_refresh) = McpRefresher::spawn(mcp_registry);

        let thread_stream: SharedThreadStream =
            PgThreadStream::spawn(pool.clone(), CancellationToken::new())
                .await
                .expect("spawn thread stream");

        let memory_store: relay_rs::memory::SharedMemoryStore =
            Arc::new(relay_rs::memory::PgMemoryStore::new(
                pool.clone(),
                clock.clone(),
                common::embedding::FakeEmbeddingProvider::shared(),
            ));

        let jwt = common::auth::test_jwt(clock.clone());
        let oauth = common::auth::test_oauth();
        let users = common::auth::user_store(pool.clone());
        let primary = seed_principal(&pool, &jwt).await;

        let state = AppState {
            queue,
            leases,
            responses,
            sessions,
            agents,
            dag,
            memory_store,
            mcp_store: mcp_store.clone(),
            mcp_refresh,
            mcp_test_rate: relay_rs::mcp::TestConnectRateLimiter::new(clock.clone()),
            thread_stream,
            pool,
            jwt,
            oauth,
            users,
            clock: clock.clone(),
            cookie_secure: false,
            memberships: std::sync::Arc::new(relay_rs::http::MembershipCache::new(clock.clone())),
        };

        Self {
            state,
            mcp_store,
            primary,
            refresher,
            db,
        }
    }

    async fn seed_mcp(&self, org: OrgId, alias_str: &str) {
        let alias = McpServerAlias::try_from(alias_str).expect("valid alias");
        let config = McpTransport::Http {
            url: McpHttpUrl::try_from(&*format!("http://localhost:9000/{alias_str}"))
                .expect("valid url"),
            headers: BTreeMap::new(),
        };
        self.mcp_store
            .create(McpServerCreate {
                org_id: org,
                created_by_user_id: self.primary.user_id,
                alias,
                config,
                description: None,
                enabled: true,
            })
            .await
            .expect("seed mcp server");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_get_mcp_servers_returns_401() {
    let h = AuthMcpHarness::new().await;
    let app = router(h.state.clone());
    let res = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/mcp-servers")
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(res.status(), axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn authenticated_new_user_sees_empty_list() {
    let h = AuthMcpHarness::new().await;
    let app = router(h.state.clone());
    let res = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/mcp-servers")
                .header("cookie", h.primary.cookie_header())
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("collect");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(json, serde_json::json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn cross_org_isolation_filters_to_caller_org() {
    let h = AuthMcpHarness::new().await;

    // Mint a *second* principal in a different org and seed one row in
    // each org via the privileged store.
    let other = seed_principal(&h.state.pool, &h.state.jwt).await;
    h.seed_mcp(h.primary.org_id, "mine").await;
    h.seed_mcp(other.org_id, "theirs").await;

    let app = router(h.state.clone());

    // Primary principal sees only their own org's row.
    let res = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/mcp-servers")
                .header("cookie", h.primary.cookie_header())
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let aliases: Vec<&str> = json
        .as_array()
        .expect("array")
        .iter()
        .map(|r| r["alias"].as_str().expect("alias"))
        .collect();
    assert_eq!(aliases, vec!["mine"]);

    // The other principal sees only theirs.
    let res = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/mcp-servers")
                .header("cookie", other.cookie_header())
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let aliases: Vec<&str> = json
        .as_array()
        .expect("array")
        .iter()
        .map(|r| r["alias"].as_str().expect("alias"))
        .collect();
    assert_eq!(aliases, vec!["theirs"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_test_connect_returns_401() {
    let h = AuthMcpHarness::new().await;
    let app = router(h.state.clone());
    let res = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/mcp-servers/test-connect")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    r#"{"config":{"type":"http","url":"http://localhost:1/"}}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(res.status(), axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connect_against_dead_url_returns_failed_outcome() {
    let h = AuthMcpHarness::new().await;
    let app = router(h.state.clone());
    let res = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/mcp-servers/test-connect")
                .header("cookie", h.primary.cookie_header())
                .header("x-csrf-token", h.primary.csrf_header())
                .header("content-type", "application/json")
                // 127.0.0.1:1 is the canonical "nothing listening" address.
                .body(axum::body::Body::from(
                    r#"{"config":{"type":"http","url":"http://127.0.0.1:1/"}}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(json["outcome"], "failed");
    assert!(!json["error"].as_str().expect("error string").is_empty());
    // No persisted side effect — the catalog stays empty.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mcp_servers")
        .fetch_one(&h.state.pool)
        .await
        .expect("count");
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connect_rate_limits_per_user() {
    let h = AuthMcpHarness::new().await;
    let app = router(h.state.clone());
    let body = r#"{"config":{"type":"http","url":"http://127.0.0.1:1/"}}"#;
    let mut last_status = axum::http::StatusCode::OK;
    // Burst past the per-minute cap. `MCP_TEST_CONNECT_PER_MIN` is 10; sending
    // 12 calls back-to-back guarantees at least one 429.
    for _ in 0..12 {
        let res = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp-servers/test-connect")
                    .header("cookie", h.primary.cookie_header())
                    .header("x-csrf-token", h.primary.csrf_header())
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");
        last_status = res.status();
    }
    assert_eq!(last_status, axum::http::StatusCode::TOO_MANY_REQUESTS);
}
