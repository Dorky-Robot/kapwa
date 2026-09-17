//! HTTP surface. Three audiences, three ways in (see `auth`):
//!
//!   * other nodes (mesh token)   GET /api/writers, GET + POST /api/log/:writer
//!   * agents (key from file)     POST /api/event, GET /api/board.txt, ...
//!   * people (Pocket ID)         GET /  — the read-only dashboard
//!
//! Binds 127.0.0.1; a cloudflared route is what makes any of it reachable.

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{Caller, Kind};
use crate::log::Event;
use crate::{oidc, render, App};

pub fn router(app: App) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/", get(dashboard))
        .route("/auth/login", get(oidc::login))
        .route("/auth/callback", get(oidc::callback))
        .route("/auth/logout", get(oidc::logout))
        .route("/api/writers", get(writers))
        .route("/api/log/:writer", get(log).post(offered))
        .route("/api/event", post(event))
        .route("/api/whoami", get(whoami))
        .route("/api/state", get(state))
        .route("/api/item/:id", get(item))
        .route("/api/peers", get(peers))
        .route("/api/board.txt", get(board_txt))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"error":"no route"}))) })
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(app)
}

fn text(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

// ── open ───────────────────────────────────────────────────────────

async fn healthz(State(app): State<App>) -> Json<Value> {
    Json(json!({"ok": true, "writer": app.cfg.writer}))
}

// ── dashboard: people, read-only ───────────────────────────────────

async fn dashboard(State(app): State<App>, caller: Caller) -> Response {
    match &caller.0 {
        Some(who) => Html(render::page(&app, who).into_string()).into_response(),
        None if app.cfg.oidc.is_some() => Redirect::to("/auth/login").into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "dashboard not configured (KAPWA_OIDC_*)",
        )
            .into_response(),
    }
}

// ── replication: nodes ─────────────────────────────────────────────

async fn writers(State(app): State<App>, caller: Caller) -> Response {
    if let Err(r) = caller.allow(&[Kind::Mesh, Kind::Agent]) {
        return r;
    }
    let m: serde_json::Map<String, Value> = app
        .logs
        .writers()
        .into_iter()
        .map(|w| (w.clone(), Value::from(app.logs.last_seq(&w))))
        .collect();
    Json(Value::Object(m)).into_response()
}

#[derive(Deserialize)]
struct Since {
    since: Option<u64>,
}

async fn log(
    State(app): State<App>,
    caller: Caller,
    Path(writer): Path<String>,
    Query(q): Query<Since>,
) -> Response {
    if let Err(r) = caller.allow(&[Kind::Mesh, Kind::Agent]) {
        return r;
    }
    if !crate::log::valid_name(&writer) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"bad writer name"})),
        )
            .into_response();
    }
    Json(app.logs.read(&writer, q.since.unwrap_or(0))).into_response()
}

/// A peer hands us lines we are behind on: its own log, or one it mirrors.
/// Same rule as a pull — only the next contiguous seq is accepted — so a
/// node that nothing can connect to takes part by reaching out.
async fn offered(
    State(app): State<App>,
    caller: Caller,
    Path(writer): Path<String>,
    Json(lines): Json<Vec<Event>>,
) -> Response {
    if let Err(r) = caller.allow(&[Kind::Mesh]) {
        return r;
    }
    if !crate::log::valid_name(&writer) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"bad writer name"})),
        )
            .into_response();
    }
    match app.logs.ingest(&writer, &lines) {
        Ok(n) => {
            if n > 0 {
                app.refresh_board();
            }
            Json(json!({"ok": true, "wrote": n, "have": app.logs.last_seq(&writer)}))
                .into_response()
        }
        Err(e) => (
            StatusCode::CONFLICT,
            Json(json!({"error": e, "have": app.logs.last_seq(&writer)})),
        )
            .into_response(),
    }
}

// ── agents ─────────────────────────────────────────────────────────

async fn event(State(app): State<App>, caller: Caller, Json(mut attrs): Json<Event>) -> Response {
    let who = match caller.allow(&[Kind::Agent]) {
        Ok(w) => w,
        Err(r) => return r,
    };
    let ok = attrs.get("kind").and_then(Value::as_str).is_some()
        && attrs.get("id").and_then(Value::as_str).is_some();
    if !ok {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"event needs `kind` and `id`"})),
        )
            .into_response();
    }
    // `by` is the key's name, never what the body claims
    attrs.insert("by".into(), Value::String(who.name.clone()));
    let id = attrs["id"].as_str().unwrap_or_default().to_string();
    match app.logs.append_own(attrs) {
        Ok(event) => {
            app.refresh_board();
            let item = app.board.read().unwrap().items.get(&id).cloned();
            Json(json!({"ok": true, "event": event, "item": item})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn whoami(caller: Caller) -> Response {
    match caller.allow(&[Kind::Mesh, Kind::Agent, Kind::User]) {
        Ok(w) => Json(w).into_response(),
        Err(r) => r,
    }
}

async fn state(State(app): State<App>, caller: Caller) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    Json(app.board.read().unwrap().clone()).into_response()
}

async fn item(State(app): State<App>, caller: Caller, Path(id): Path<String>) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    match app.board.read().unwrap().items.get(&id) {
        Some(i) => Json(i).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error":"no such item"}))).into_response(),
    }
}

async fn peers(State(app): State<App>, caller: Caller) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    Json(app.peers.lock().unwrap().clone()).into_response()
}

async fn board_txt(State(app): State<App>, caller: Caller) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    text(render::board(&app))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app(tmp: &std::path::Path) -> App {
        let agents = tmp.join("agents");
        std::fs::write(
            &agents,
            "# comment\nclaude:claude-key:worker:portfolio,a2p\n",
        )
        .unwrap();
        App::new(Config {
            writer: "test".into(),
            dir: tmp.join("data"),
            port: 0,
            peers: vec![],
            mesh_token: Some("mesh-secret".into()),
            agents_file: agents,
            public_url: "http://127.0.0.1:0".into(),
            secret_key_base: None,
            oidc: None,
        })
    }

    async fn call(app: &App, req: Request<Body>) -> (StatusCode, String) {
        let resp = router(app.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&body).to_string())
    }

    fn get_req(path: &str, bearer: Option<&str>) -> Request<Body> {
        let mut b = Request::get(path);
        if let Some(t) = bearer {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(Body::empty()).unwrap()
    }

    fn post_req(path: &str, bearer: &str, body: Value) -> Request<Body> {
        Request::post(path)
            .header("authorization", format!("Bearer {bearer}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn healthz_is_open_and_replication_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        assert_eq!(
            call(&app, get_req("/healthz", None)).await.0,
            StatusCode::OK
        );
        assert_eq!(
            call(&app, get_req("/api/writers", None)).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, get_req("/api/writers", Some("wrong"))).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, get_req("/api/writers", Some("mesh-secret")))
                .await
                .0,
            StatusCode::OK
        );
    }

    fn line(writer: &str, seq: u64) -> Value {
        json!({"kind":"note","id":"x","writer":writer,"seq":seq,"by":writer,
               "at":format!("2026-09-17T10:00:0{seq}.000Z")})
    }

    #[tokio::test]
    async fn a_peer_may_hand_over_lines_under_the_same_rule_as_a_pull() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let post = |tok: &str, w: &str, body: Value| post_req(&format!("/api/log/{w}"), tok, body);
        // contiguous lines land
        let (st, body) = call(
            &app,
            post(
                "mesh-secret",
                "leaf",
                json!([line("leaf", 1), line("leaf", 2)]),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (v["wrote"].as_u64(), v["have"].as_u64()),
            (Some(2), Some(2))
        );
        // handed over again: nothing written, nothing broken
        let (_, body) = call(
            &app,
            post(
                "mesh-secret",
                "leaf",
                json!([line("leaf", 1), line("leaf", 2)]),
            ),
        )
        .await;
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["wrote"], 0);
        // a gap is not written, and the answer says where to resume
        let (_, body) = call(&app, post("mesh-secret", "leaf", json!([line("leaf", 5)]))).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (v["wrote"].as_u64(), v["have"].as_u64()),
            (Some(0), Some(2))
        );
        // nobody hands me my own log, and an agent key is not a node
        assert_eq!(
            call(&app, post("mesh-secret", "test", json!([line("test", 1)])))
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            call(&app, post("claude-key", "leaf", json!([line("leaf", 3)])))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn mesh_token_reads_but_cannot_write() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let (st, _) = call(
            &app,
            post_req("/api/event", "mesh-secret", json!({"kind":"note","id":"x"})),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn agent_writes_and_by_is_the_key_name() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let (st, body) = call(
            &app,
            post_req(
                "/api/event",
                "claude-key",
                json!({"kind":"create","id":"k1","title":"K","by":"impostor"}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["event"]["by"], "claude");
        assert_eq!(v["item"]["created_by"], "claude");
    }

    #[tokio::test]
    async fn dashboard_is_503_not_open_without_oidc_but_renders_for_a_key() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        assert_eq!(
            call(&app, get_req("/", None)).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let (st, body) = call(&app, get_req("/", Some("claude-key"))).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.contains("read-only"));
    }
}
