//! HTTP surface. Three audiences, three ways in (see `auth`):
//!
//!   * other nodes (mesh token)   GET /api/writers, GET + POST /api/log/:writer
//!   * agents (key from file)     POST /api/event, GET /api/prime.txt, ...
//!   * people (Pocket ID)         GET /  — the read-only board
//!
//! Open to anyone: /healthz, /api/protocol (how to use this), and sign-in.
//! Binds 127.0.0.1; a tunnel route is what makes any of it reachable.

use std::hash::{Hash, Hasher};

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{Caller, Kind, Who};
use crate::board::{clean_topic, verb};
use crate::log::Event;
use crate::{oidc, render, App};

pub fn router(app: App) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/protocol", get(protocol))
        .route("/", get(dashboard))
        .route("/auth/login", get(oidc::login))
        .route("/auth/callback", get(oidc::callback))
        .route("/auth/logout", get(oidc::logout))
        .route("/auth/bye", get(oidc::bye))
        .route("/api/writers", get(writers))
        .route("/api/log/:writer", get(log).post(offered))
        .route("/api/event", post(event))
        .route("/api/whoami", get(whoami))
        .route("/api/prime.txt", get(prime_txt))
        .route("/api/board.txt", get(board_txt))
        .route("/api/mine", get(mine))
        .route("/api/state", get(state))
        .route("/api/item/:id", get(item))
        .route("/api/peers", get(peers))
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"no route; GET /api/protocol says how this works"})),
            )
        })
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(app)
}

fn text(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

fn bad(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

// ── open ───────────────────────────────────────────────────────────

async fn healthz(State(app): State<App>) -> Json<Value> {
    Json(json!({"ok": true, "writer": app.cfg.writer}))
}

/// The manual. Open on purpose: it holds instructions, not content, and an
/// agent that finds the URL should be able to learn the rest.
async fn protocol(State(app): State<App>) -> Response {
    text(render::protocol(&app))
}

// ── the board: people, read-only ───────────────────────────────────

async fn dashboard(State(app): State<App>, caller: Caller) -> Response {
    match &caller.0 {
        Some(who) => Html(render::page(&app, who).into_string()).into_response(),
        None if app.cfg.oidc.is_some() => Redirect::to("/auth/login").into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "no sign-in configured here (KAPWA_OIDC_*); agents: GET /api/protocol",
        )
            .into_response(),
    }
}

// ── sync: nodes ────────────────────────────────────────────────────

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
        return bad(StatusCode::BAD_REQUEST, "bad writer name");
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
        return bad(StatusCode::BAD_REQUEST, "bad writer name");
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

/// Which item does `given` mean? An exact id, else the one id it prefixes.
fn resolve(app: &App, given: &str) -> Result<Option<String>, String> {
    let st = app.board.read().unwrap();
    if st.items.contains_key(given) {
        return Ok(Some(given.to_string()));
    }
    let hits: Vec<&String> = st.items.keys().filter(|k| k.starts_with(given)).collect();
    match hits.len() {
        0 => Ok(None),
        1 => Ok(Some(hits[0].clone())),
        n => Err(format!(
            "`{given}` is ambiguous: {n} items start with it ({})",
            hits.iter()
                .take(4)
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// A short id for an item nobody named. Not a secret and not a proof, just
/// unlikely to collide; it gives way to the event's own id once events are
/// signed.
fn mint(app: &App, seed: &str) -> String {
    let st = app.board.read().unwrap();
    for salt in 0u32.. {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (seed, &app.cfg.writer, crate::log::now(), salt).hash(&mut h);
        let id = format!("{:05x}", h.finish() & 0xfffff);
        if !st.items.contains_key(&id) {
            return id;
        }
    }
    unreachable!()
}

async fn event(State(app): State<App>, caller: Caller, Json(mut attrs): Json<Event>) -> Response {
    let who = match caller.allow(&[Kind::Agent]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    let Some(kind) = attrs.get("kind").and_then(Value::as_str).map(String::from) else {
        return bad(
            StatusCode::BAD_REQUEST,
            "an event needs a `kind`: say · take · drop · done · ask",
        );
    };
    let given = attrs
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let id = match (given, verb(&kind)) {
        (Some(g), v) => match resolve(&app, &g) {
            Ok(Some(id)) => id,
            // a `say` may name a new item; anything else must find one
            Ok(None) if v == Some("say") || v.is_none() => g,
            Ok(None) => {
                return bad(
                    StatusCode::NOT_FOUND,
                    &format!("no item `{g}` here (yet?); `say` makes one"),
                )
            }
            Err(e) => return bad(StatusCode::CONFLICT, &e),
        },
        (None, Some("say")) => mint(
            &app,
            attrs.get("text").and_then(Value::as_str).unwrap_or(""),
        ),
        (None, _) => {
            return bad(
                StatusCode::BAD_REQUEST,
                "which item? give an `id` (a unique prefix will do)",
            )
        }
    };
    attrs.insert("id".into(), Value::String(id.clone()));
    // `by` is the key's name (and session tag), never what the body claims
    attrs.insert("by".into(), Value::String(who.name.clone()));
    match app.logs.append_own(attrs) {
        Ok(event) => {
            app.refresh_board();
            let item = app.board.read().unwrap().items.get(&id).cloned();
            Json(json!({"ok": true, "id": id, "event": event, "item": item})).into_response()
        }
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn whoami(caller: Caller) -> Response {
    match caller.allow(&[Kind::Mesh, Kind::Agent, Kind::User]) {
        Ok(w) => Json(w).into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct Scope {
    /// topics, comma-separated; absent means the key's own, else everything
    t: Option<String>,
}

fn topics(q: &Scope, who: &Who) -> Vec<String> {
    match &q.t {
        Some(t) => t.split(',').filter_map(clean_topic).collect(),
        None => who.topics.clone().unwrap_or_default(),
    }
}

async fn prime_txt(State(app): State<App>, caller: Caller, Query(q): Query<Scope>) -> Response {
    match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => text(render::prime(&app, w, &topics(&q, w))),
        Err(r) => r,
    }
}

async fn board_txt(State(app): State<App>, caller: Caller, Query(q): Query<Scope>) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    // the whole board unless asked to narrow it: a key's own topics scope
    // what it is primed with, not what it may look at
    let ts: Vec<String> =
        q.t.as_deref()
            .map(|t| t.split(',').filter_map(clean_topic).collect())
            .unwrap_or_default();
    text(render::board(&app, &ts))
}

async fn mine(State(app): State<App>, caller: Caller, Query(q): Query<Scope>) -> Response {
    match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => {
            let m = render::mine(&app, w, &topics(&q, w));
            Json(json!({"you": w.name, "asked": m.asked, "held": m.held, "said_to": m.said_to, "open": m.open})).into_response()
        }
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
    match resolve(&app, &id) {
        Ok(Some(id)) => Json(app.board.read().unwrap().items.get(&id)).into_response(),
        Ok(None) => bad(StatusCode::NOT_FOUND, "no such item"),
        Err(e) => bad(StatusCode::CONFLICT, &e),
    }
}

async fn peers(State(app): State<App>, caller: Caller) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    Json(app.peers.lock().unwrap().clone()).into_response()
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
            "# comment\nclaude:claude-key:worker:roof,fence\nana:ana-key:lead:*\n",
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

    async fn say(app: &App, key: &str, body: Value) -> Value {
        let (st, body) = call(app, post_req("/api/event", key, body)).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    #[tokio::test]
    async fn the_manual_and_healthz_are_open_and_everything_else_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        assert_eq!(
            call(&app, get_req("/healthz", None)).await.0,
            StatusCode::OK
        );
        let (st, body) = call(&app, get_req("/api/protocol", None)).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.contains("say") && body.contains("take"));
        for p in [
            "/api/writers",
            "/api/board.txt",
            "/api/prime.txt",
            "/api/mine",
            "/api/state",
        ] {
            assert_eq!(
                call(&app, get_req(p, None)).await.0,
                StatusCode::UNAUTHORIZED,
                "{p}"
            );
        }
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
        json!({"kind":"say","id":"x","writer":writer,"seq":seq,"by":writer,
               "at":format!("2026-09-17T10:00:0{seq}.000Z")})
    }

    #[tokio::test]
    async fn a_peer_may_hand_over_lines_under_the_same_rule_as_a_pull() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let post = |tok: &str, w: &str, body: Value| post_req(&format!("/api/log/{w}"), tok, body);
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
    async fn the_mesh_token_syncs_but_cannot_speak() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let (st, _) = call(
            &app,
            post_req(
                "/api/event",
                "mesh-secret",
                json!({"kind":"say","text":"x"}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn say_mints_an_id_and_by_is_the_key_never_the_body() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let v = say(
            &app,
            "ana-key",
            json!({"kind":"say","text":"Roof leaks","by":"impostor","t":["roof"]}),
        )
        .await;
        let id = v["id"].as_str().unwrap().to_string();
        assert_eq!(id.len(), 5);
        assert_eq!(
            (v["event"]["by"].as_str(), v["item"]["created_by"].as_str()),
            (Some("ana"), Some("ana"))
        );
        // a unique prefix is enough to mean it
        let v = say(&app, "claude-key", json!({"kind":"take","id":&id[..3]})).await;
        assert_eq!(
            (v["id"].as_str(), v["item"]["owner"].as_str()),
            (Some(id.as_str()), Some("claude"))
        );
        // but a verb about nothing is refused, with a hint
        let (st, body) = call(
            &app,
            post_req(
                "/api/event",
                "claude-key",
                json!({"kind":"take","id":"zzzz"}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert!(body.contains("say"));
    }

    #[tokio::test]
    async fn a_tag_signs_one_session_of_a_key_and_only_ever_as_a_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let req = |tag: &str| {
            Request::post("/api/event")
                .header("authorization", "Bearer claude-key")
                .header("x-kapwa-tag", tag)
                .header("content-type", "application/json")
                .body(Body::from(json!({"kind":"say","text":"hi"}).to_string()))
                .unwrap()
        };
        let (_, body) = call(&app, req("ab12")).await;
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["event"]["by"],
            "claude/ab12"
        );
        // a tag that tries to be a name is ignored, not honoured
        let (_, body) = call(&app, req("../ana")).await;
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["event"]["by"],
            "claude"
        );
    }

    #[tokio::test]
    async fn prime_is_what_involves_you_and_a_key_s_topics_scope_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        say(
            &app,
            "ana-key",
            json!({"kind":"say","id":"roofing","text":"Roof leaks","t":["roof"]}),
        )
        .await;
        say(
            &app,
            "ana-key",
            json!({"kind":"say","id":"taxes","text":"File the taxes","t":["money"]}),
        )
        .await;
        say(
            &app,
            "ana-key",
            json!({"kind":"say","id":"plan","text":"Which plan?"}),
        )
        .await;
        say(
            &app,
            "ana-key",
            json!({"kind":"ask","id":"plan","to":"claude","text":"A or B?"}),
        )
        .await;
        let (_, prime) = call(&app, get_req("/api/prime.txt", Some("claude-key"))).await;
        assert!(
            prime.contains("asked of you (1)") && prime.contains("A or B?"),
            "{prime}"
        );
        assert!(prime.contains("roofing"), "{prime}");
        assert!(
            !prime.contains("taxes"),
            "claude watches roof and fence only:\n{prime}"
        );
        // asking for a topic overrides the key's own
        let (_, prime) = call(&app, get_req("/api/prime.txt?t=money", Some("claude-key"))).await;
        assert!(prime.contains("taxes") && !prime.contains("roofing"));
        // the board is never narrowed unless asked
        let (_, board) = call(&app, get_req("/api/board.txt", Some("claude-key"))).await;
        assert!(board.contains("taxes") && board.contains("roofing"));
        let (_, mine) = call(&app, get_req("/api/mine", Some("claude-key"))).await;
        assert_eq!(
            serde_json::from_str::<Value>(&mine).unwrap()["asked"][0]["id"],
            "plan"
        );
    }

    #[tokio::test]
    async fn signing_out_lands_somewhere_that_does_not_sign_you_back_in() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        let resp = router(app.clone())
            .oneshot(get_req("/auth/logout", None))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(resp.headers()["location"], "/auth/bye");
        let (st, body) = call(&app, get_req("/auth/bye", None)).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.contains("Signed out") && body.contains("/auth/login"));
    }

    #[tokio::test]
    async fn the_board_is_503_not_open_without_sign_in_but_renders_for_a_key() {
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
