//! Who is asking. Three kinds of caller, told apart by how they identify:
//!
//! - `Mesh`: another node, presenting the shared `KAPWA_MESH_TOKEN`.
//!   May read logs to replicate. Never writes.
//! - `Agent`: a key from the agents file (`name:token:role:topics`).
//!   The token is the name: `by` on every event comes from it. One key is
//!   often many sessions at once, so a caller may add `X-Kapwa-Tag: ab12`
//!   and sign as `name/ab12`. The tag is only ever a suffix of the key's
//!   own name, so nobody can sign as someone else.
//! - `User`: a person, signed in through Pocket ID. Reads the board, and
//!   writes the three verbs a participant needs to keep a promise: `say`,
//!   `take`, `done`. Not `ask` — a person with a queue to answer does not
//!   need a faster way to add to it — and nothing that hands out keys.
//!
//! Fail closed: with no mesh token configured, no node can replicate; with
//! no agents file, no agent can write. Nothing is open by default except
//! `/healthz` and the login flow.

use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::extract::cookie::PrivateCookieJar;
use constant_time_eq::constant_time_eq;
use serde::Serialize;
use serde_json::json;

use crate::App;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Mesh,
    Agent,
    User,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Kind::Mesh => "mesh",
            Kind::Agent => "agent",
            Kind::User => "user",
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Who {
    pub kind: Kind,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// the topics this key cares about, from the agents file; `*` or none
    /// means everything
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topics: Option<Vec<String>>,
}

fn valid_tag(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 32
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Extractor: `who` is `Some` if the request identified itself.
///
/// `csrf_ok` is about *how* it identified itself. A bearer token is carried
/// on purpose by whoever holds it, so a request bearing one is one its
/// sender meant to make. A cookie is sent by the browser whether or not the
/// person meant it, so a write over a cookie has to repeat a token only our
/// own pages can read.
pub struct Caller {
    pub who: Option<Who>,
    pub csrf_ok: bool,
    /// This session's token, to hand back to the one session that already
    /// has it. Never for an agent, never logged, never on a page a stranger
    /// can reach.
    pub csrf: Option<String>,
}

#[async_trait]
impl FromRequestParts<App> for Caller {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<Self, Self::Rejection> {
        if let Some(tok) = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
        {
            let mut who = from_token(app, tok.trim());
            if let Some(w) = who.as_mut().filter(|w| w.kind == Kind::Agent) {
                if let Some(tag) = parts
                    .headers
                    .get("x-kapwa-tag")
                    .and_then(|v| v.to_str().ok())
                    .map(str::trim)
                    .filter(|t| valid_tag(t))
                {
                    w.name = format!("{}/{tag}", w.name);
                }
            }
            return Ok(Caller {
                who,
                csrf_ok: true,
                csrf: None,
            });
        }
        let jar = PrivateCookieJar::from_request_parts(parts, app)
            .await
            .unwrap_or_else(|e| match e {});
        let session = crate::oidc::session(&jar);
        let csrf_ok = session.as_ref().is_some_and(|s| {
            !s.csrf.is_empty()
                && parts
                    .headers
                    .get("x-kapwa-csrf")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|got| constant_time_eq(got.as_bytes(), s.csrf.as_bytes()))
        });
        let csrf = session
            .as_ref()
            .map(|s| s.csrf.clone())
            .filter(|c| !c.is_empty());
        Ok(Caller {
            csrf,
            who: session.map(|s| Who {
                kind: Kind::User,
                // the name a person is known by here, not the address they
                // sign in with: `by` goes into an append-only log that is
                // read by everyone, and an ask is addressed to a name
                name: s.name,
                role: None,
                topics: None,
            }),
            csrf_ok,
        })
    }
}

impl Caller {
    /// The caller, if it is one of `kinds`; otherwise the 401/403 to send.
    /// (The Err is a full Response on purpose: handlers return it as-is.)
    #[allow(clippy::result_large_err)]
    pub fn allow(&self, kinds: &[Kind]) -> Result<&Who, Response> {
        match &self.who {
            None => Err(deny(
                StatusCode::UNAUTHORIZED,
                "identify yourself: Authorization: Bearer <token> (the token is your name)",
            )),
            Some(w) if kinds.contains(&w.kind) => Ok(w),
            Some(w) => Err(deny(
                StatusCode::FORBIDDEN,
                &format!(
                    "{} may not do this; needs one of: {}",
                    w.kind,
                    kinds
                        .iter()
                        .map(|k| k.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }

    /// The same, for anything that writes. Everything `allow` asks, plus:
    /// a cookie had to bring a token with it.
    #[allow(clippy::result_large_err)]
    pub fn allow_write(&self, kinds: &[Kind]) -> Result<&Who, Response> {
        let who = self.allow(kinds)?;
        if !self.csrf_ok {
            return Err(deny(
                StatusCode::FORBIDDEN,
                "a write over a cookie must repeat the session's token: send it as X-Kapwa-CSRF (GET /api/whoami says yours)",
            ));
        }
        Ok(who)
    }

    /// For a write that arrives as a form, where a header is not on offer:
    /// the token comes in the body instead, and is checked the same way.
    pub fn csrf_matches(&self, given: &str) -> bool {
        self.csrf
            .as_deref()
            .is_some_and(|c| !c.is_empty() && constant_time_eq(c.as_bytes(), given.as_bytes()))
    }
}

fn deny(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn from_token(app: &App, tok: &str) -> Option<Who> {
    if let Some(mesh) = &app.cfg.mesh_token {
        if constant_time_eq(mesh.as_bytes(), tok.as_bytes()) {
            return Some(Who {
                kind: Kind::Mesh,
                name: "mesh".into(),
                role: None,
                topics: None,
            });
        }
    }
    agents(app)
        .into_iter()
        .find(|(_, t)| constant_time_eq(t.as_bytes(), tok.as_bytes()))
        .map(|(w, _)| w)
}

/// Agents file, re-read per lookup so a minted key works without a
/// restart. Lines are `name:token:role:topics`; `#` starts a comment.
pub fn agents(app: &App) -> Vec<(Who, String)> {
    let Ok(body) = std::fs::read_to_string(&app.cfg.agents_file) else {
        return vec![];
    };
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut p = l.split(':');
            let name = p.next()?.to_string();
            let token = p.next()?.to_string();
            let role = p.next().unwrap_or("worker").to_string();
            // `*` (or nothing) means every topic, which is what no list means
            let topics: Vec<String> = p
                .next()
                .unwrap_or("")
                .split(',')
                .filter_map(crate::board::clean_topic)
                .filter(|t| t != "*")
                .collect();
            Some((
                Who {
                    kind: Kind::Agent,
                    name,
                    role: Some(role),
                    topics: Some(topics),
                },
                token,
            ))
        })
        .collect()
}
