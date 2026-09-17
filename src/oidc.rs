//! Dashboard sign-in against the network's Pocket ID (`id.felixflor.es`,
//! `id.homesforsalebymonica.com`). Standard authorization-code OpenID
//! Connect with PKCE; the result is a small session in an encrypted
//! cookie. Who may sign in is decided in Pocket ID (the client's allowed
//! groups), not here.
//!
//! None of the crypto is ours: verifying an id token is signature checking
//! against a rotating key set, and `openidconnect` does it. Lifted from
//! everyday-vet-admin's auth.rs, minus the multi-face origin logic.
//!
//! Unconfigured (no `KAPWA_OIDC_*`) means the dashboard is off: `/`
//! answers 503 rather than falling open. Discovery of the provider is
//! retried in the background, so a node that boots while Pocket ID is
//! unreachable still comes up and starts replicating.

use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar, SameSite};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::reqwest::async_http_client;
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use serde::{Deserialize, Serialize};

use crate::App;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub subject: String,
    pub name: String,
    pub email: Option<String>,
}

/// The in-flight half of a sign-in, held in a short cookie rather than in
/// server memory, so a restart mid-login is not an error.
#[derive(Serialize, Deserialize)]
struct Pending {
    csrf: String,
    nonce: String,
    verifier: String,
}

const SESSION: &str = "_kapwa_session";
const PENDING: &str = "_kapwa_pending";

pub async fn discover(cfg: &crate::config::Oidc, public_url: &str) -> Result<CoreClient> {
    let issuer = IssuerUrl::new(cfg.issuer.clone()).context("issuer url")?;
    let meta = CoreProviderMetadata::discover_async(issuer, async_http_client)
        .await
        .context("could not reach the identity provider")?;
    Ok(CoreClient::from_provider_metadata(
        meta,
        ClientId::new(cfg.client_id.clone()),
        Some(ClientSecret::new(cfg.client_secret.clone())),
    )
    .set_redirect_uri(
        RedirectUrl::new(format!("{public_url}/auth/callback")).context("redirect url")?,
    ))
}

/// Keep trying discovery until it works; the dashboard is off until then.
pub fn discover_in_background(app: App) {
    let Some(cfg) = app.cfg.oidc.clone() else {
        return;
    };
    tokio::spawn(async move {
        let mut wait = 5u64;
        loop {
            match discover(&cfg, &app.cfg.public_url).await {
                Ok(client) => {
                    *app.oidc.write().unwrap() = Some(std::sync::Arc::new(client));
                    // the provider's own sign-out, from its own metadata: never a
                    // path we assume
                    let meta = format!(
                        "{}/.well-known/openid-configuration",
                        cfg.issuer.trim_end_matches('/')
                    );
                    if let Ok(resp) = app.http.get(&meta).send().await {
                        if let Ok(v) = resp.json::<serde_json::Value>().await {
                            *app.end_session.write().unwrap() =
                                v["end_session_endpoint"].as_str().map(String::from);
                        }
                    }
                    tracing::info!("oidc: discovered {}", cfg.issuer);
                    return;
                }
                Err(e) => {
                    tracing::warn!("oidc: {e:#}; retrying in {wait}s");
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    wait = (wait * 2).min(300);
                }
            }
        }
    });
}

pub fn session(jar: &PrivateCookieJar) -> Option<Session> {
    serde_json::from_str(jar.get(SESSION)?.value()).ok()
}

fn client(app: &App) -> Option<std::sync::Arc<CoreClient>> {
    app.oidc.read().unwrap().clone()
}

fn not_configured(app: &App) -> Response {
    let msg = if app.cfg.oidc.is_some() {
        "dashboard sign-in is not ready: the identity provider has not answered discovery yet"
    } else {
        "dashboard not configured (KAPWA_OIDC_*)"
    };
    (StatusCode::SERVICE_UNAVAILABLE, msg).into_response()
}

pub async fn login(State(app): State<App>, jar: PrivateCookieJar) -> Response {
    let Some(client) = client(&app) else {
        return not_configured(&app);
    };
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, csrf, nonce) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("email".into()))
        .add_scope(Scope::new("profile".into()))
        .set_pkce_challenge(challenge)
        .url();
    let pending = Pending {
        csrf: csrf.secret().clone(),
        nonce: nonce.secret().clone(),
        verifier: verifier.secret().clone(),
    };
    let jar = jar.add(crumb(
        &app,
        PENDING,
        serde_json::to_string(&pending).unwrap_or_default(),
        600,
    ));
    (jar, Redirect::to(url.as_str())).into_response()
}

#[derive(Deserialize)]
pub struct Returned {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub async fn callback(
    State(app): State<App>,
    jar: PrivateCookieJar,
    Query(back): Query<Returned>,
) -> Response {
    // no pending cookie: the flow was already consumed (a reload) or the
    // cookie never came back — start over rather than fail
    if jar.get(PENDING).is_none() {
        return Redirect::to("/auth/login").into_response();
    }
    match finish(&app, &jar, back).await {
        Ok(session) => {
            let jar = jar.remove(gone(PENDING)).add(crumb(
                &app,
                SESSION,
                serde_json::to_string(&session).unwrap_or_default(),
                30 * 24 * 3600,
            ));
            (jar, Redirect::to("/")).into_response()
        }
        Err(e) => {
            tracing::warn!("sign-in did not complete: {e:#}");
            (
                StatusCode::UNAUTHORIZED,
                jar.remove(gone(PENDING)),
                format!("sign-in failed: {e}"),
            )
                .into_response()
        }
    }
}

async fn finish(app: &App, jar: &PrivateCookieJar, back: Returned) -> Result<Session> {
    if let Some(e) = back.error {
        anyhow::bail!("the identity provider refused: {e}");
    }
    let client = client(app).context("no identity provider")?;
    let code = back.code.context("no code came back")?;
    let state = back.state.context("no state came back")?;
    let pending: Pending = serde_json::from_str(
        jar.get(PENDING)
            .context("this sign-in did not start here")?
            .value(),
    )
    .context("could not read the pending sign-in")?;
    if state != pending.csrf {
        anyhow::bail!("the sign-in state did not match");
    }
    let tokens = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier))
        .request_async(async_http_client)
        .await
        .map_err(|e| {
            // the provider's own words go to the log (never to the browser):
            // "invalid client secret" and "code already used" need different fixes
            let why = match &e {
                openidconnect::RequestTokenError::ServerResponse(r) => format!("{r:?}"),
                openidconnect::RequestTokenError::Parse(_, body) => {
                    String::from_utf8_lossy(body).chars().take(300).collect()
                }
                other => other.to_string(),
            };
            tracing::warn!("token endpoint refused: {why}");
            anyhow::anyhow!("could not trade the code for tokens")
        })?;
    let id_token = tokens.id_token().context("no id token in the response")?;
    let claims = id_token
        .claims(&client.id_token_verifier(), &Nonce::new(pending.nonce))
        .context("the id token did not verify")?;
    let name = claims
        .preferred_username()
        .map(|n| n.as_str().to_string())
        .or_else(|| {
            claims
                .name()
                .and_then(|n| n.get(None))
                .map(|n| n.as_str().to_string())
        })
        .unwrap_or_else(|| claims.subject().as_str().to_string());
    Ok(Session {
        subject: claims.subject().as_str().to_string(),
        name,
        email: claims.email().map(|e| e.as_str().to_string()),
    })
}

/// Sign out of kapwa, and land somewhere that does not sign you back in.
/// Going to `/` would bounce to the provider, which still has its own
/// session and would return you silently: a logout that does nothing.
pub async fn logout(jar: PrivateCookieJar) -> impl IntoResponse {
    (jar.remove(gone(SESSION)), Redirect::to("/auth/bye"))
}

pub async fn bye(State(app): State<App>) -> Response {
    let issuer = app.cfg.oidc.as_ref().map(|o| o.issuer.clone());
    let end = app.end_session.read().unwrap().clone();
    axum::response::Html(
        maud::html! {
            (maud::DOCTYPE)
            html lang="en" {
                head {
                    meta charset="utf-8";
                    meta name="viewport" content="width=device-width,initial-scale=1";
                    title { "kapwa · signed out" }
                    style { "body{font:15px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;margin:4rem auto;max-width:34rem;padding:0 1rem;color:#222;background:#fafafa}a{color:#27c}p.dim{color:#888}" }
                }
                body {
                    h1 style="font-size:1.1rem" { "Signed out of kapwa." }
                    p { a href="/auth/login" { "Sign in again" } }
                    @if let (Some(end), Some(issuer)) = (&end, &issuer) {
                        p class="dim" {
                            "You are still signed in at " (issuer.trim_start_matches("https://")) ", so signing in again won't ask for your passkey. "
                            a href=(end) { "Sign out there too" } "."
                        }
                    }
                }
            }
        }
        .into_string(),
    )
    .into_response()
}

/// A cookie is identified by its name *and its path*, so a removal that does
/// not name the path it was set on clears nothing: the browser keeps the one
/// at `/` and the person stays signed in. Every removal goes through here.
fn gone(name: &'static str) -> Cookie<'static> {
    let mut c = Cookie::from(name);
    c.set_path("/");
    c
}

fn crumb(app: &App, name: &'static str, value: String, secs: i64) -> Cookie<'static> {
    let mut c = Cookie::new(name, value);
    c.set_path("/");
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    // the tunnel terminates TLS; local dev is plain http
    c.set_secure(app.cfg.public_url.starts_with("https://"));
    c.set_max_age(cookie::time::Duration::seconds(secs));
    c
}
