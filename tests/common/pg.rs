//! Schema-per-test Postgres harness.
//!
//! Each [`TestDb::fresh`] call mints a unique schema name, runs `migrations/` into it
//! by way of a pool whose `search_path` is pinned to that schema, and drops the
//! schema on teardown. Suitable for parallel `cargo test` runs against a single
//! long-running container brought up by the project's `docker-compose.yml`.
//!
//! - Env: set `DATABASE_URL` to override the default
//!   `postgres://relay:relay@localhost:5432/relay`.
//! - Cleanup is RAII: schemas drop synchronously when [`TestDb`] goes out of scope.
//!   The [`Drop`] impl uses `tokio::task::block_in_place` + `Handle::block_on`, so
//!   tests **must** run on the multi-threaded runtime
//!   (`#[tokio::test(flavor = "multi_thread")]`); a current-thread runtime panics on
//!   `block_in_place`. Stray schemas can be reaped manually with `psql`:
//!   `SELECT 'DROP SCHEMA "' || nspname || '" CASCADE;' FROM pg_namespace WHERE nspname LIKE 'relay_test_%';`

use std::time::Duration;

use relay_rs::agents::{AgentId, AgentName, AgentSystemPrompt, DefaultAgentSeed, PgAgentStore};
use relay_rs::clock::SystemClock;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://relay:relay@localhost:5432/relay";
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Owns a freshly-migrated, schema-isolated Postgres pool. Use `pool` to construct
/// the unit under test (`PgSessionStore`, `PgPromptQueue`, `PgResponseHub`).
///
/// `default_agent_id` is the row seeded by [`TestDb::fresh`] — every fresh db
/// has exactly one default agent so `sessions.agent_id NOT NULL REFERENCES
/// agents(id)` can be satisfied by tests without ceremony.
pub struct TestDb {
    /// Pool whose connections are pinned to this test's schema via
    /// `SET search_path TO "<schema>", public`.
    pub pool: PgPool,
    pub default_agent_id: AgentId,
    schema: String,
    admin: PgPool,
}

impl TestDb {
    /// Mint a unique schema, build a pool pinned to it, and run the project's
    /// migrations into it. Panics on any setup failure — tests cannot proceed
    /// without a working database.
    pub async fn fresh() -> Self {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.into());

        // Admin pool keeps a connection open for the lifetime of the test so we can
        // CREATE / DROP SCHEMA without interfering with the test pool.
        let admin: PgPool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&url)
            .await
            .expect("admin pool connect");

        let schema = format!("relay_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA \"{schema}\""))
            .execute(&admin)
            .await
            .expect("create schema");

        let pinned_schema = schema.clone();
        let pool: PgPool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(move |conn, _meta| {
                let stmt = format!("SET search_path TO \"{pinned_schema}\", public");
                Box::pin(async move {
                    use sqlx::Executor as _;
                    conn.execute(stmt.as_str()).await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .expect("test pool connect");

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("run migrations");

        // Seed a default agent so `sessions.agent_id` (NOT NULL REFERENCES
        // agents) can be satisfied by tests calling `sessions.create(id)`.
        let agents = PgAgentStore::new(pool.clone(), SystemClock::shared());
        let default_agent_id = agents
            .seed_default(DefaultAgentSeed {
                name: AgentName::try_from("test-default").expect("valid name"),
                system_prompt: AgentSystemPrompt::try_from("test default prompt")
                    .expect("valid prompt"),
            })
            .await
            .expect("seed default agent");

        Self {
            pool,
            default_agent_id,
            schema,
            admin,
        }
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let schema = std::mem::take(&mut self.schema);
        if schema.is_empty() {
            return;
        }
        // Drop runs while the test future is still being torn down on a runtime worker;
        // `Handle::try_current()` therefore succeeds on every supported test path.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let admin = self.admin.clone();
        let pool = self.pool.clone();
        // `block_in_place` requires the multi-threaded runtime; tests pin
        // `flavor = "multi_thread"` so block_on the cleanup synchronously here.
        tokio::task::block_in_place(move || {
            handle.block_on(async move {
                pool.close().await;
                let _ = tokio::time::timeout(
                    TEARDOWN_TIMEOUT,
                    sqlx::query(&format!("DROP SCHEMA \"{schema}\" CASCADE")).execute(&admin),
                )
                .await;
                admin.close().await;
            });
        });
    }
}
