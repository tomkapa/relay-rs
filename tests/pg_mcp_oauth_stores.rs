//! Postgres roundtrip tests for the MCP OAuth tables (R3 — phase C).
//!
//! Exercises:
//!   - `mcp_oauth_clients` upsert + idempotent re-upsert (no duplicate row).
//!   - Encrypted `client_secret` decrypts back to the same plaintext.
//!   - `mcp_oauth_pending` insert + one-shot `consume` (replay fails).
//!   - Expired pending rows are surfaced as `None` from `consume`.

#![allow(clippy::expect_used)]

use std::sync::Arc;

use relay_rs::clock::SystemClock;
use relay_rs::crypto::OrgEncryptor;
use relay_rs::mcp::oauth::{
    McpOAuthClientStore, McpOAuthPendingStore, NewOAuthClient, PendingAuthorizationWrite,
    PgMcpOAuthClientStore, PgMcpOAuthPendingStore,
};
use relay_rs::mcp::{
    McpHttpUrl, McpServerAlias, McpServerCreate, McpServerStore, McpTransport, PgMcpServerStore,
};
use relay_rs::types::SecretString;

mod common;
use common::pg::TestDb;

fn encryptor() -> Arc<OrgEncryptor> {
    Arc::new(OrgEncryptor::for_test([9u8; 32]))
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_client_upsert_then_read_returns_decrypted_secret() {
    let db = TestDb::fresh().await;
    let clock = SystemClock::shared();
    let store = PgMcpOAuthClientStore::new(db.pool.clone(), clock, encryptor());

    let row = store
        .upsert(NewOAuthClient {
            org_id: db.default_org_id,
            issuer: "https://issuer.example".into(),
            client_id: "client-xyz".into(),
            client_secret: Some(SecretString::try_from("sekret-1".to_owned()).expect("valid")),
            authorization_endpoint: "https://issuer.example/auth".into(),
            token_endpoint: "https://issuer.example/token".into(),
            registration_client_uri: None,
            registration_access_token: None,
            token_endpoint_auth_method: "client_secret_basic".into(),
            scope: Some("read write".into()),
        })
        .await
        .expect("upsert");
    assert_eq!(row.client_id, "client-xyz");
    assert_eq!(row.scope.as_deref(), Some("read write"));
    let secret = row.client_secret.as_ref().expect("client_secret present");
    assert_eq!(secret.expose(), "sekret-1");

    let again = store
        .read(db.default_org_id, "https://issuer.example")
        .await
        .expect("read")
        .expect("present");
    assert_eq!(again.client_id, "client-xyz");
    assert_eq!(
        again.client_secret.as_ref().expect("secret").expose(),
        "sekret-1"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_client_upsert_is_idempotent_per_issuer() {
    let db = TestDb::fresh().await;
    let clock = SystemClock::shared();
    let store = PgMcpOAuthClientStore::new(db.pool.clone(), clock, encryptor());

    let first = store
        .upsert(NewOAuthClient {
            org_id: db.default_org_id,
            issuer: "https://issuer.example".into(),
            client_id: "first-id".into(),
            client_secret: Some(SecretString::try_from("first-secret".to_owned()).expect("valid")),
            authorization_endpoint: "https://issuer.example/auth".into(),
            token_endpoint: "https://issuer.example/token".into(),
            registration_client_uri: None,
            registration_access_token: None,
            token_endpoint_auth_method: "client_secret_basic".into(),
            scope: None,
        })
        .await
        .expect("first upsert");

    let second = store
        .upsert(NewOAuthClient {
            org_id: db.default_org_id,
            issuer: "https://issuer.example".into(),
            client_id: "second-id".into(),
            client_secret: Some(SecretString::try_from("second-secret".to_owned()).expect("valid")),
            authorization_endpoint: "https://issuer.example/auth".into(),
            token_endpoint: "https://issuer.example/token".into(),
            registration_client_uri: None,
            registration_access_token: None,
            token_endpoint_auth_method: "client_secret_basic".into(),
            scope: None,
        })
        .await
        .expect("second upsert");
    // Idempotent — the second call returns the first row, not a new one.
    assert_eq!(first.client_id, second.client_id);
    assert_eq!(first.client_id, "first-id");
}

async fn seed_server(db: &TestDb) -> relay_rs::mcp::McpServerId {
    let server_store = PgMcpServerStore::new(db.pool.clone(), SystemClock::shared());
    let row = server_store
        .create(McpServerCreate {
            org_id: db.default_org_id,
            created_by_user_id: db.default_user_id,
            alias: McpServerAlias::try_from("oauth").expect("alias"),
            config: McpTransport::Http {
                url: McpHttpUrl::try_from("http://localhost:9000").expect("url"),
            },
            description: None,
            enabled: true,
        })
        .await
        .expect("seed server");
    row.id
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_pending_insert_then_consume_returns_row() {
    let db = TestDb::fresh().await;
    let server_id = seed_server(&db).await;
    let clock = SystemClock::shared();
    let store = PgMcpOAuthPendingStore::new(db.pool.clone(), clock.clone());
    let now = clock.now_utc();
    store
        .insert(PendingAuthorizationWrite {
            state: "a".repeat(40),
            server_id,
            user_id: db.default_user_id,
            org_id: db.default_org_id,
            issuer: "https://issuer.example".into(),
            pkce_verifier: "v".repeat(43),
            redirect_to: Some("/settings".into()),
            expires_at: now + chrono::Duration::seconds(120),
        })
        .await
        .expect("insert");
    let row = store
        .consume(&"a".repeat(40), now)
        .await
        .expect("consume")
        .expect("row present");
    assert_eq!(row.server_id, server_id);
    assert_eq!(row.issuer, "https://issuer.example");
    // Second consume returns None — the row was deleted on the first read.
    let dup = store
        .consume(&"a".repeat(40), now)
        .await
        .expect("second consume");
    assert!(dup.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_pending_expired_rows_yield_none() {
    let db = TestDb::fresh().await;
    let server_id = seed_server(&db).await;
    let clock = SystemClock::shared();
    let store = PgMcpOAuthPendingStore::new(db.pool.clone(), clock.clone());
    let now = clock.now_utc();
    store
        .insert(PendingAuthorizationWrite {
            state: "b".repeat(40),
            server_id,
            user_id: db.default_user_id,
            org_id: db.default_org_id,
            issuer: "https://issuer.example".into(),
            pkce_verifier: "v".repeat(43),
            redirect_to: None,
            expires_at: now - chrono::Duration::seconds(1),
        })
        .await
        .expect("insert");
    let row = store.consume(&"b".repeat(40), now).await.expect("consume");
    assert!(row.is_none(), "expired rows must not be returned");
}
