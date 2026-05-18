//! Trait-contract tests for [`relay_rs::mcp::PgMcpServerStore`]. Each test owns its
//! own schema via `TestDb::fresh` so they can run in parallel.

#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use relay_rs::clock::SystemClock;
use relay_rs::mcp::{
    DiscoveredTool, McpError, McpHealthUpdate, McpHttpUrl, McpServerAlias, McpServerCreate,
    McpServerId, McpServerStore, McpServerUpdate, McpTransport, PgMcpServerStore,
};

mod common;
use common::pg::TestDb;

fn store(db: &TestDb) -> Arc<PgMcpServerStore> {
    Arc::new(PgMcpServerStore::new(
        db.pool.clone(),
        SystemClock::shared(),
    ))
}

fn http_transport(url: &str) -> McpTransport {
    McpTransport::Http {
        url: McpHttpUrl::try_from(url).expect("valid url"),
        headers: BTreeMap::new(),
    }
}

fn alias(s: &str) -> McpServerAlias {
    McpServerAlias::try_from(s).expect("valid alias")
}

#[tokio::test(flavor = "multi_thread")]
async fn create_read_roundtrip() {
    let db = TestDb::fresh().await;
    let store = store(&db);

    let payload = McpServerCreate {
        org_id: db.default_org_id,
        created_by_user_id: db.default_user_id,
        alias: alias("every"),
        config: http_transport("http://localhost:9000/"),
        description: None,
        enabled: true,
    };
    let row = store.create(payload).await.expect("create");
    let read = store.read(row.id, db.default_org_id).await.expect("read");
    assert_eq!(read.id, row.id);
    assert_eq!(read.alias.as_str(), "every");
    assert!(read.enabled);
    assert_eq!(read.last_seen_at, None);
    assert_eq!(read.last_error, None);
    assert_eq!(read.discovered_tools, None);
    assert_eq!(read.created_by_user_id, db.default_user_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_alias_is_rejected() {
    let db = TestDb::fresh().await;
    let store = store(&db);

    store
        .create(McpServerCreate {
            org_id: db.default_org_id,
            created_by_user_id: db.default_user_id,
            alias: alias("dup"),
            config: http_transport("http://localhost:9000/"),
            description: None,
            enabled: true,
        })
        .await
        .expect("first create");
    let err = store
        .create(McpServerCreate {
            org_id: db.default_org_id,
            created_by_user_id: db.default_user_id,
            alias: alias("dup"),
            config: http_transport("http://localhost:9001/"),
            description: None,
            enabled: true,
        })
        .await
        .expect_err("second create");
    assert!(matches!(err, McpError::AliasTaken(a) if a == "dup"));
}

#[tokio::test(flavor = "multi_thread")]
async fn list_orders_by_alias() {
    let db = TestDb::fresh().await;
    let store = store(&db);
    for name in ["zeta", "alpha", "mid"] {
        store
            .create(McpServerCreate {
                org_id: db.default_org_id,
                created_by_user_id: db.default_user_id,
                alias: alias(name),
                config: http_transport(&format!("http://localhost:9000/{name}")),
                description: None,
                enabled: name != "mid",
            })
            .await
            .expect("create");
    }
    let all = store.list().await.expect("list");
    let names: Vec<&str> = all.iter().map(|r| r.alias.as_str()).collect();
    assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    let enabled = store.list_enabled().await.expect("list_enabled");
    let enabled_names: Vec<&str> = enabled.iter().map(|r| r.alias.as_str()).collect();
    assert_eq!(enabled_names, vec!["alpha", "zeta"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn update_changes_alias_and_config() {
    let db = TestDb::fresh().await;
    let store = store(&db);
    let row = store
        .create(McpServerCreate {
            org_id: db.default_org_id,
            created_by_user_id: db.default_user_id,
            alias: alias("first"),
            config: http_transport("http://localhost:9000/"),
            description: None,
            enabled: true,
        })
        .await
        .expect("create");
    let updated = store
        .update(
            row.id,
            db.default_org_id,
            McpServerUpdate {
                alias: Some(alias("renamed")),
                config: Some(http_transport("http://localhost:9100/")),
                description: None,
                enabled: Some(false),
            },
        )
        .await
        .expect("update");
    assert_eq!(updated.alias.as_str(), "renamed");
    assert!(!updated.enabled);
    let read = store.read(row.id, db.default_org_id).await.expect("read");
    let McpTransport::Http { url, .. } = &read.config;
    assert_eq!(url.as_str(), "http://localhost:9100/");
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_returns_not_found_after() {
    let db = TestDb::fresh().await;
    let store = store(&db);
    let row = store
        .create(McpServerCreate {
            org_id: db.default_org_id,
            created_by_user_id: db.default_user_id,
            alias: alias("temp"),
            config: http_transport("http://localhost:9000/"),
            description: None,
            enabled: true,
        })
        .await
        .expect("create");
    store
        .delete(row.id, db.default_org_id)
        .await
        .expect("delete");
    let err = store
        .read(row.id, db.default_org_id)
        .await
        .expect_err("read after delete");
    assert!(matches!(err, McpError::NotFound(_)));
    let err = store
        .delete(row.id, db.default_org_id)
        .await
        .expect_err("delete again");
    assert!(matches!(err, McpError::NotFound(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn update_health_persists_discovered_tools() {
    let db = TestDb::fresh().await;
    let store = store(&db);
    let row = store
        .create(McpServerCreate {
            org_id: db.default_org_id,
            created_by_user_id: db.default_user_id,
            alias: alias("health"),
            config: http_transport("http://localhost:9000/"),
            description: None,
            enabled: true,
        })
        .await
        .expect("create");
    let now = chrono::Utc::now();
    let discovered = vec![DiscoveredTool {
        remote_name: "echo".into(),
        prefixed_name: "mcp_health_echo".into(),
        description: Some("echoes input".into()),
    }];
    store
        .update_health(
            row.id,
            db.default_org_id,
            McpHealthUpdate {
                last_seen_at: Some(now),
                last_error: None,
                discovered_tools: Some(discovered.clone()),
            },
        )
        .await
        .expect("update_health");
    let read = store.read(row.id, db.default_org_id).await.expect("read");
    assert!(read.last_seen_at.is_some());
    assert_eq!(read.last_error, None);
    let tools = read.discovered_tools.expect("tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].remote_name, "echo");
    assert_eq!(tools[0].prefixed_name, "mcp_health_echo");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_id_returns_not_found_on_read_update_delete() {
    let db = TestDb::fresh().await;
    let store = store(&db);
    let id = McpServerId::new();
    assert!(matches!(
        store.read(id, db.default_org_id).await.expect_err("read"),
        McpError::NotFound(_)
    ));
    assert!(matches!(
        store
            .update(id, db.default_org_id, McpServerUpdate::default())
            .await
            .expect_err("update"),
        McpError::NotFound(_)
    ));
    assert!(matches!(
        store
            .delete(id, db.default_org_id)
            .await
            .expect_err("delete"),
        McpError::NotFound(_)
    ));
}
