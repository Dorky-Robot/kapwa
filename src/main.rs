//! kapwa — what participants owe each other.
//!
//! One binary. `kapwa serve` runs this machine's node; the client verbs
//! (say · take · drop · done · ask) come next.
//!
//! One process per machine. Agents on the machine talk to it over
//! 127.0.0.1; it pulls every other node's log and folds them all into one
//! board; people read that board through the network's Pocket ID.
//! Protocol in PROTOCOL.md.

mod auth;
mod board;
mod config;
mod log;
mod oidc;
mod puller;
mod render;
mod routes;

use std::sync::{Arc, Mutex, RwLock};

use axum_extra::extract::cookie::Key;
use base64::Engine as _;

use crate::config::Config;

pub struct Inner {
    pub cfg: Config,
    pub logs: log::Logs,
    pub board: RwLock<board::State>,
    pub peers: puller::Peers,
    pub oidc: RwLock<Option<Arc<openidconnect::core::CoreClient>>>,
    pub http: reqwest::Client,
    pub key: Key,
}

#[derive(Clone)]
pub struct App(pub Arc<Inner>);

impl std::ops::Deref for App {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl axum::extract::FromRef<App> for Key {
    fn from_ref(app: &App) -> Key {
        app.key.clone()
    }
}

impl App {
    pub fn new(cfg: Config) -> App {
        // the session cookie key: ≥64 bytes from the env, else per-boot
        // (fine when there is no dashboard to keep a session for)
        let key = cfg
            .secret_key_base
            .as_ref()
            .and_then(|b| {
                base64::engine::general_purpose::STANDARD
                    .decode(b)
                    .ok()
                    .or_else(|| Some(b.as_bytes().to_vec()))
            })
            .filter(|b| b.len() >= 64)
            .map(|b| Key::from(&b))
            .unwrap_or_else(Key::generate);
        let logs = log::Logs::open(&cfg.dir, &cfg.writer);
        let app = App(Arc::new(Inner {
            logs,
            board: RwLock::new(board::State::default()),
            peers: Arc::new(Mutex::new(Default::default())),
            oidc: RwLock::new(None),
            http: reqwest::Client::builder()
                .user_agent(concat!("kapwa/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client"),
            key,
            cfg,
        }));
        app.refresh_board();
        app
    }

    pub fn refresh_board(&self) {
        let events = self.logs.all();
        let st = board::State {
            items: board::fold(&events),
            writers: self
                .logs
                .writers()
                .into_iter()
                .map(|w| (w.clone(), self.logs.last_seq(&w)))
                .collect(),
            events: events.len(),
            built_at: log::now(),
        };
        *self.board.write().unwrap() = st;
    }
}

const HELP: &str = "kapwa — what participants owe each other

  kapwa serve        run this machine's node (127.0.0.1:3410)
  kapwa --version

  the client verbs (say · take · drop · done · ask) are not built yet;
  until then: curl, with a key from ~/.config/kapwa/agents. see PROTOCOL.md
";

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("serve") => serve(),
        Some("--version" | "-V") => {
            println!("kapwa {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        None | Some("help" | "--help" | "-h") => {
            print!("{HELP}");
            Ok(())
        }
        Some(other) => {
            eprintln!("kapwa: unknown command `{other}`\n");
            eprint!("{HELP}");
            std::process::exit(2);
        }
    }
}

#[tokio::main]
async fn serve() -> anyhow::Result<()> {
    // the node's env file, unless the environment already says otherwise
    let env_file = std::env::var("KAPWA_ENV_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| config::home().join(".config/kapwa/env"));
    config::load_env_file(&env_file);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .init();

    let cfg = Config::from_env()?;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let app = App::new(cfg);
    tracing::info!(
        "kapwa {} · writer {} · {} · peers {} · mesh token {} · agents {} · dashboard {}",
        env!("CARGO_PKG_VERSION"),
        app.cfg.writer,
        addr,
        app.cfg.peers.len(),
        if app.cfg.mesh_token.is_some() {
            "set"
        } else {
            "UNSET (no replication)"
        },
        app.cfg.agents_file.display(),
        if app.cfg.oidc.is_some() {
            "oidc"
        } else {
            "off"
        },
    );

    oidc::discover_in_background(app.clone());
    puller::supervise(app.clone());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, routes::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
