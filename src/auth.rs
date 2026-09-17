//! Who is asking. Three kinds of caller, told apart by how they identify:
//!
//! - `Mesh`: another node, presenting the shared `KAPWA_MESH_TOKEN`.
//!   May read logs to replicate. Never writes.
//! - `Agent`: a key from the agents file (`name:token:role:products`).
//!   The token is the name: `by` on every event comes from it.
//! - `User`: a person, signed in through Pocket ID. Reads the dashboard.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub products: Option<Vec<String>>,
}

/// Extractor: `Caller(Some(who))` if the request identified itself.
pub struct Caller(pub Option<Who>);

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
            return Ok(Caller(from_token(app, tok.trim())));
        }
        let jar = PrivateCookieJar::from_request_parts(parts, app)
            .await
            .unwrap_or_else(|e| match e {});
        Ok(Caller(crate::oidc::session(&jar).map(|s| Who {
            kind: Kind::User,
            name: s.email.unwrap_or(s.name),
            role: None,
            products: None,
        })))
    }
}

impl Caller {
    /// The caller, if it is one of `kinds`; otherwise the 401/403 to send.
    /// (The Err is a full Response on purpose: handlers return it as-is.)
    #[allow(clippy::result_large_err)]
    pub fn allow(&self, kinds: &[Kind]) -> Result<&Who, Response> {
        match &self.0 {
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
                products: None,
            });
        }
    }
    agents(app)
        .into_iter()
        .find(|(_, t)| constant_time_eq(t.as_bytes(), tok.as_bytes()))
        .map(|(w, _)| w)
}

/// Agents file, re-read per lookup so a minted key works without a
/// restart. Lines are `name:token:role:products`; `#` starts a comment.
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
            let products = p
                .next()
                .unwrap_or("")
                .split(',')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            Some((
                Who {
                    kind: Kind::Agent,
                    name,
                    role: Some(role),
                    products: Some(products),
                },
                token,
            ))
        })
        .collect()
}
