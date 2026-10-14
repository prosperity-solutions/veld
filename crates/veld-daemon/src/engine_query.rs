//! The engine's read-only queries over HTTP: `veld status` and `veld logs`.
//!
//! Plain JSON, no streaming: a follower polls `/api/engine/logs` with
//! `after_id`, exactly as the CLI used to poll the database. Errors carry the
//! engine's user-facing message verbatim as `{"error": …}` with a 400 — the CLI
//! prints it unchanged.

use std::path::PathBuf;

use axum::Router;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use serde::Deserialize;
use veld_engine::api::LogsQuery;

use super::engine::{into_response, project_root_of};

pub(crate) fn routes() -> Router {
    Router::new()
        .route("/api/engine/status", get(status))
        .route("/api/engine/logs", get(logs))
        // Logs and outputs are what a rebound page would come to read. Every route
        // goes ABOVE this line: `route_layer` wraps only the routes added before it.
        .route_layer(axum::middleware::from_fn(super::engine::host_guard))
}

#[derive(Deserialize)]
struct StatusParams {
    project_root: PathBuf,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    outputs: bool,
}

async fn status(Query(p): Query<StatusParams>) -> Response {
    into_response(status_inner(p).await)
}

async fn status_inner(
    p: StatusParams,
) -> Result<veld_engine::api::StatusReport, (StatusCode, String)> {
    let root = project_root_of(&p.project_root)?;
    crate::offload::blocking(move || veld_engine::query::status(&root, p.name, p.outputs))
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

async fn logs(Query(mut q): Query<LogsQuery>) -> Response {
    into_response(logs_inner(&mut q).await)
}

async fn logs_inner(q: &mut LogsQuery) -> Result<veld_engine::api::LogsPage, (StatusCode, String)> {
    q.project_root = project_root_of(&q.project_root)?;
    let q = q.clone();
    crate::offload::blocking(move || veld_engine::query::logs(q))
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;

    async fn get_json(uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = routes()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn queries_answer_with_the_engines_own_messages() {
        let _env = crate::feedback_server::lock_db_env();
        let dir = tempfile::TempDir::new().unwrap();
        // SAFETY: lock_db_env serialises every writer of these variables.
        unsafe { std::env::set_var("VELD_DB_PATH", dir.path().join("t.db")) };
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("veld.json"), // root-config-gate-ok: a test fixture writing the file
            r#"{ "schemaVersion": "3", "name": "t", "nodes": {} }"#,
        )
        .unwrap();
        let root = project.to_string_lossy();

        // No scope: rejected during extraction.
        assert_eq!(
            get_json("/api/engine/status").await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            get_json("/api/engine/logs").await.0,
            StatusCode::BAD_REQUEST
        );

        // A directory that is not a project.
        let (code, _) = get_json("/api/engine/status?project_root=/nonexistent-veld-dir").await;
        assert_eq!(code, StatusCode::BAD_REQUEST);

        // A project with no runs: the CLI's verbatim message.
        for path in ["status", "logs"] {
            let (code, body) = get_json(&format!("/api/engine/{path}?project_root={root}")).await;
            assert_eq!(code, StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(
                body["error"], "No environments found. Start one with `veld start`.",
                "{path}"
            );
        }
        let (_, body) = get_json(&format!(
            "/api/engine/logs?project_root={root}&run=deadbeef"
        ))
        .await;
        assert_eq!(
            body["error"],
            "No run matches id prefix 'deadbeef' (see `veld runs`)."
        );
        unsafe { std::env::remove_var("VELD_DB_PATH") };
    }
}
