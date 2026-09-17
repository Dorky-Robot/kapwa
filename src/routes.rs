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
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{Caller, Kind, Who};
use crate::board::{clean_topic, verb};
use crate::log::Event;
use crate::{oidc, render, try_ui, App};

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
        .route("/api/feed", get(feed))
        .route("/api/day", get(day))
        .route("/api/day.txt", get(day_txt))
        .route("/api/stats", get(stats))
        .route("/api/stats.txt", get(stats_txt))
        .route("/api/topics", get(topics_in_use))
        .route("/api/live", get(live))
        // a sandbox, until one of them is picked
        .route("/try/", get(|s, c| try_page(s, c, "index")))
        .route("/try/constellation", get(|s, c| try_page(s, c, "c")))
        .route("/try/rail", get(|s, c| try_page(s, c, "r")))
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"no route; GET / says how this works"})),
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

/// The same instructions, at a path an agent can guess. Open on purpose: it
/// holds instructions, not content.
async fn protocol(State(app): State<App>) -> Response {
    text(render::how(&app, false))
}

// ── the board: people, read-only ───────────────────────────────────

/// The root is the front door, and what it serves depends on who knocked.
///
/// Signed in, or holding a key: the board. Otherwise the instructions — as a
/// page for a browser, as plain text for everything else. It deliberately
/// does not bounce a stranger to the identity provider: a passkey page is no
/// use to an agent, and a person who has never been here deserves to be told
/// what this is before being asked who they are.
async fn dashboard(
    State(app): State<App>,
    caller: Caller,
    headers: axum::http::HeaderMap,
) -> Response {
    if let Some(who) = &caller.0 {
        return Html(render::page(&app, who).into_string()).into_response();
    }
    let wants_html = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"));
    if wants_html {
        Html(render::front_door(&app, app.cfg.oidc.is_some()).into_string()).into_response()
    } else {
        text(render::how(&app, false))
    }
}

// ── sync: nodes ────────────────────────────────────────────────────

async fn writers(State(app): State<App>, caller: Caller, Query(q): Query<Since>) -> Response {
    if let Err(r) = caller.allow(&[Kind::Mesh, Kind::Agent]) {
        return r;
    }
    // hold it open until something lands, so a peer hears within a round
    // trip instead of within its polling interval
    if let Some(secs) = q.wait.filter(|s| *s > 0) {
        app.changed(std::time::Duration::from_secs(secs.min(60)))
            .await;
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
    /// seconds to hold the request open until something changes. Waiting
    /// costs one held connection; asking again and again costs a request
    /// per interval per peer, forever.
    wait: Option<u64>,
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
    // A new item with no topic would land in the commons, where it is on
    // everyone's board and in nobody's. So it goes to its author's own
    // topic instead: a namespace you get by construction, and never a
    // shared one you did not choose.
    let new_item = !app.board.read().unwrap().items.contains_key(&id);
    let untagged = crate::board::topics_of(&attrs).is_empty();
    if new_item && untagged && verb(&kind) == Some("say") {
        attrs.insert("t".into(), json!([crate::board::base(&who.name)]));
    }
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

/// What to show this caller when they did not say. A key watches the topics
/// it was given, and always its own: whatever it writes untagged lands
/// there, so it never loses sight of its own work. A key with no topics
/// listed watches everything.
fn topics(q: &Scope, who: &Who) -> Vec<String> {
    match &q.t {
        Some(t) => t.split(',').filter_map(clean_topic).collect(),
        None => {
            let mut t = who.topics.clone().unwrap_or_default();
            if !t.is_empty() {
                let own = crate::board::base(&who.name).to_string();
                if !t.contains(&own) {
                    t.push(own);
                }
            }
            t
        }
    }
}

/// Every topic in use, commonest first. An open vocabulary costs synonyms,
/// and this is what keeps the bill down: look before you invent a word.
async fn topics_in_use(State(app): State<App>, caller: Caller) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    let st = app.board.read().unwrap();
    let mut n: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for i in st.items.values() {
        for t in &i.topics {
            let e = n.entry(t.clone()).or_default();
            e.0 += 1;
            if i.status != "done" {
                e.1 += 1;
            }
        }
    }
    let mut rows: Vec<(String, usize, usize)> =
        n.into_iter().map(|(t, (a, o))| (t, a, o)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    text(
        rows.iter()
            .map(|(t, all, open)| format!("  {:<18} {all:>3} items, {open} open", format!("#{t}")))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
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

/// Asked for, never assumed: the whole board's day unless `t` narrows it,
/// the same rule `board.txt` follows.
fn asked_for(t: &Option<String>) -> Vec<String> {
    t.as_deref()
        .map(|t| t.split(',').filter_map(clean_topic).collect())
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct DayQ {
    /// `today` · `yesterday` · `2026-09-17`; absent means today
    on: Option<String>,
    t: Option<String>,
}

fn on_day(q: &DayQ) -> Result<chrono::NaiveDate, Response> {
    let given = q.on.clone().unwrap_or_else(|| "today".into());
    crate::day::parse_on(&given).ok_or_else(|| {
        bad(
            StatusCode::BAD_REQUEST,
            &format!("`{given}` is not a day: today · yesterday · YYYY-MM-DD"),
        )
    })
}

async fn day(State(app): State<App>, caller: Caller, Query(q): Query<DayQ>) -> Response {
    let who = match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    let on = match on_day(&q) {
        Ok(d) => d,
        Err(r) => return r,
    };
    let st = app.board.read().unwrap().clone();
    let steps = crate::day::day(&st, on, &who.name, &asked_for(&q.t));
    Json(json!({"on": on.to_string(), "you": who.name, "steps": steps})).into_response()
}

async fn day_txt(State(app): State<App>, caller: Caller, Query(q): Query<DayQ>) -> Response {
    let who = match caller.allow(&[Kind::Agent, Kind::User]) {
        Ok(w) => w.clone(),
        Err(r) => return r,
    };
    match on_day(&q) {
        Ok(on) => text(render::day(&app, &who, on, &asked_for(&q.t))),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct StatsQ {
    /// how far back to count; a week unless asked otherwise
    days: Option<i64>,
    t: Option<String>,
}

async fn stats(State(app): State<App>, caller: Caller, Query(q): Query<StatsQ>) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    let st = app.board.read().unwrap().clone();
    Json(crate::metrics::of(
        &st,
        q.days.unwrap_or(7),
        &asked_for(&q.t),
    ))
    .into_response()
}

async fn stats_txt(State(app): State<App>, caller: Caller, Query(q): Query<StatsQ>) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    text(render::stats(&app, q.days.unwrap_or(7), &asked_for(&q.t)))
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

/// Ways of looking at the same fold, to be chosen by seeing them. Behind
/// the same door as the board: it is the same data.
async fn try_page(State(app): State<App>, caller: Caller, which: &'static str) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    Html(
        match which {
            "c" => try_ui::constellation(&app),
            "r" => try_ui::rail(&app),
            _ => try_ui::index(&app),
        }
        .into_string(),
    )
    .into_response()
}

/// What just happened, newest first. An agent coming back after a while
/// wants this, not the whole board.
async fn feed(State(app): State<App>, caller: Caller, Query(q): Query<Limit>) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    let rows = render::feed(&app, q.limit.unwrap_or(30).min(200));
    if q.format.as_deref() == Some("json") {
        let v: Vec<Value> = rows
            .into_iter()
            .map(|(at, by, verb, id, text)| json!({"at": at, "by": by, "verb": verb, "id": id, "text": text}))
            .collect();
        return Json(v).into_response();
    }
    text(
        rows.into_iter()
            .map(|(at, by, verb, id, text)| {
                format!(
                    "{:>4}  {:<18} {:<5} {:<8} {text}",
                    render::ago(&at),
                    by,
                    verb,
                    id
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
}

/// A change happened. One line per change, nothing in it: whoever cares
/// asks for what they want. Costs a held connection and no polling.
async fn live(State(app): State<App>, caller: Caller) -> Response {
    if let Err(r) = caller.allow(&[Kind::Agent, Kind::User]) {
        return r;
    }
    let stream = async_stream::stream! {
        // say hello at once, so a client knows it is connected
        yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data("open"));
        loop {
            // a heartbeat every 20s keeps a proxy from closing a quiet stream
            let changed = app.changed(std::time::Duration::from_secs(20)).await;
            yield Ok(axum::response::sse::Event::default().data(if changed { "board" } else { "tick" }));
        }
    };
    axum::response::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

#[derive(Deserialize)]
struct Limit {
    limit: Option<usize>,
    format: Option<String>,
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
        // the root and its alias hand a stranger the whole manual
        for p in ["/", "/api/protocol"] {
            let (st, body) = call(&app, get_req(p, None)).await;
            assert_eq!(st, StatusCode::OK, "{p}");
            assert!(
                body.contains("say") && body.contains("take") && body.contains("/api/event"),
                "{p}"
            );
            assert!(
                body.contains("ask them for these"),
                "{p} must say what needs a person"
            );
        }
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

    /// A session lives in a cookie set at `Path=/`. A removal that does not
    /// say the same path leaves that cookie in place, and the next visit is
    /// still signed in — which is exactly what happened.
    #[tokio::test]
    async fn signing_out_removes_the_session_cookie_at_the_path_it_was_set_on() {
        use axum_extra::extract::cookie::{Cookie, PrivateCookieJar};
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());

        // a session cookie, exactly as a completed sign-in leaves one
        let mut c = Cookie::new("_kapwa_session", r#"{"subject":"s","name":"someone"}"#);
        c.set_path("/");
        let jar: PrivateCookieJar = PrivateCookieJar::new(app.key.clone()).add(c);
        let set = jar.into_response();
        let sent: String = set
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
            .collect::<Vec<_>>()
            .join("; ");

        let req = Request::get("/auth/logout")
            .header("cookie", &sent)
            .body(Body::empty())
            .unwrap();
        let resp = router(app.clone()).oneshot(req).await.unwrap();
        let removal: Vec<String> = resp
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .filter(|v| v.starts_with("_kapwa_session="))
            .collect();
        assert_eq!(removal.len(), 1, "sign-out must clear the session cookie");
        let removal = &removal[0];
        assert!(
            removal.contains("Path=/"),
            "removal needs the path it was set on: {removal}"
        );
        assert!(
            removal.contains("Max-Age=0") || removal.contains("Expires=Thu, 01 Jan 1970"),
            "removal must expire it: {removal}"
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
    async fn the_root_is_the_board_for_a_key_and_the_manual_for_a_stranger() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(tmp.path());
        // a key: the board
        let (st, body) = call(&app, get_req("/", Some("claude-key"))).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.contains("read-only"));
        // a browser: a page that says what this is, never a bounce to sign-in
        let req = Request::get("/")
            .header("accept", "text/html,application/xhtml+xml")
            .body(Body::empty())
            .unwrap();
        let (st, body) = call(&app, req).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.contains("<pre>") && body.contains("If you are an agent"));
        // anything else: the text itself
        let req = Request::get("/")
            .header("accept", "*/*")
            .body(Body::empty())
            .unwrap();
        let (st, body) = call(&app, req).await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.starts_with("kapwa ·"));
    }
}
