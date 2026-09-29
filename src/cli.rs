//! The client half of the one binary: a thin keyboard over the local node's
//! HTTP surface. It holds no rules of its own — two copies of the rules
//! disagree quietly — so everything it prints, the node decided.
//!
//! Made for two readers at once: a person at a terminal, and an agent that
//! ran `kapwa --help` a second ago. Text on a TTY, JSON when piped, exit
//! codes that mean something, and nothing interactive.

use std::io::IsTerminal;
use std::time::Duration;

use serde_json::{json, Value};

pub const HELP: &str = "\
kapwa — what participants owe each other

  kapwa                     the board              [--t topic]
  kapwa say \"…\"             a new item             [--t topic] [--to who] [--p P1] [--as name]
  kapwa say <id> \"…\"        a note on one
  kapwa take <id>           it's mine              [--wait]
  kapwa drop <id>           not mine anymore
  kapwa done <id> [\"…\"]     finished, or decided against, because …  [yours, or asked of you]
  kapwa ask <id> \"…\"        someone must answer    [--to who]
  kapwa ask \"…\" --to who    …about something new

  kapwa is the mesh's record, and the way to reach whoever you cannot reach
  directly: bots, other harnesses, people. sessions that can message each
  other (Claude Code: SendMessage) talk there, and do not --to each other.

  kapwa day                 what happened today, and who did it  [--on <day>] [--t topic]
  kapwa stats               where the work waited                [--days N] [--t topic]
  kapwa topics              what topics are in use; look before inventing
  kapwa mine                what waits on me, what I hold
  kapwa show <id>           one item and its history
  kapwa prime               what an agent should know right now       [--wait]
  kapwa mine --hook         a line, but only if somebody tapped you
  kapwa how                 how this works, for someone new (open to anyone)
  kapwa whoami

  kapwa join [name]         get a key of your own on this machine
  kapwa join --with <invite>   …or from anywhere, on an invitation
  kapwa invite <name>       vouch for someone (a lead only) [--role] [--t] [--hours]

  kapwa serve               run this machine's node
  kapwa setup claude        print the hook that primes every session

  <id>     any unique prefix will do, as with git
  --t      a topic; repeat it, or comma-separate. an item can have many.
           with none, a new item goes to a topic named after you, so it is
           never on everybody's board by accident
  --wait   (take) wait one sync, then answer CLAIMED · LOST to x
           (prime) hold until what involves you changes, or a few minutes pass
  --tag    sign as <name>/<tag>: one session of many (or KAPWA_TAG).
           call yourself something a person can read — `--tag dashboard`
           beats a hash. the hook names you after where you are working
  --me     which of your keys: ~/.config/kapwa/keys/<name> (or KAPWA_ME)
  --on     (day) today · yesterday · 2026-09-17
  --days   (stats) how far back to count; a week by default
  --json   force JSON; it is already the default when piped

  key      KAPWA_KEY, else ~/.config/kapwa/key
  node     --url, else KAPWA_URL, else http://127.0.0.1:3410
  exit     0 ok · 1 no · 2 usage · 3 not identified

examples
  kapwa say \"Roof leaks over the back door\" --t roof
  kapwa take 7f3 --wait
  kapwa ask 7f3 --to felix \"patch it, or replace the flashing?\"
  kapwa mine --json | jq -r '.asked[].id'
  kapwa day --on yesterday --t roof
";

#[derive(Default)]
struct Args {
    pos: Vec<String>,
    t: Vec<String>,
    to: Option<String>,
    p: Option<String>,
    name: Option<String>,
    tag: Option<String>,
    me: Option<String>,
    on: Option<String>,
    days: Option<String>,
    json: bool,
    wait: bool,
    hook: bool,
    url: Option<String>,
    with: Option<String>,
    role: Option<String>,
    hours: Option<i64>,
}

fn parse(raw: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = raw.iter();
    while let Some(x) = it.next() {
        let mut val = |flag: &str| it.next().cloned().ok_or(format!("{flag} needs a value"));
        match x.as_str() {
            "--t" | "-t" => a.t.extend(val("--t")?.split(',').map(String::from)),
            "--to" => a.to = Some(val("--to")?),
            "--p" => a.p = Some(val("--p")?),
            "--as" => a.name = Some(val("--as")?),
            "--tag" => a.tag = Some(val("--tag")?),
            "--me" => a.me = Some(val("--me")?),
            "--on" => a.on = Some(val("--on")?),
            "--days" => a.days = Some(val("--days")?),
            "--json" => a.json = true,
            "--wait" => a.wait = true,
            "--hook" => a.hook = true,
            "--url" => a.url = Some(val("--url")?),
            "--with" => a.with = Some(val("--with")?),
            "--role" => a.role = Some(val("--role")?),
            "--hours" => {
                a.hours = Some(
                    val("--hours")?
                        .parse()
                        .map_err(|_| "--hours wants a number".to_string())?,
                )
            }
            "-m" => a.pos.push(val("-m")?),
            f if f.starts_with("--") => return Err(format!("unknown flag {f}")),
            _ => a.pos.push(x.clone()),
        }
    }
    Ok(a)
}

struct Node {
    pub http: reqwest::Client,
    pub url: String,
    key: Option<String>,
    tag: Option<String>,
}

enum Fail {
    /// exit 1: the node said no, or isn't there
    No(String),
    /// exit 3: no key, or the node doesn't know it
    Who(String),
}

impl Node {
    fn new(a: &Args) -> Node {
        let env = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let url = a
            .url
            .clone()
            .or_else(|| env("KAPWA_URL"))
            .unwrap_or_else(|| {
                format!(
                    "http://127.0.0.1:{}",
                    env("KAPWA_PORT").unwrap_or_else(|| "3410".into())
                )
            });
        // on one OS account every key is readable by every process, so
        // which one you sign with is a choice, not a wall
        let file = match a.me.clone().or_else(|| env("KAPWA_ME")) {
            Some(me) if crate::log::valid_name(&me) => format!(".config/kapwa/keys/{me}"),
            _ => ".config/kapwa/key".to_string(),
        };
        let key = env("KAPWA_KEY").or_else(|| {
            std::fs::read_to_string(crate::config::home().join(&file))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
        Node {
            http: reqwest::Client::new(),
            url: url.trim_end_matches('/').to_string(),
            key,
            tag: a.tag.clone().or_else(|| env("KAPWA_TAG")),
        }
    }

    pub async fn send(&self, req: reqwest::RequestBuilder, open: bool) -> Result<String, Fail> {
        let mut req = req.timeout(Duration::from_secs(15));
        if !open {
            let Some(k) = &self.key else {
                return Err(Fail::Who(
                    "no key: set KAPWA_KEY, or put one in ~/.config/kapwa/key".into(),
                ));
            };
            req = req.bearer_auth(k);
            if let Some(t) = &self.tag {
                req = req.header("x-kapwa-tag", t);
            }
        }
        let resp = req.send().await.map_err(|_| {
            Fail::No(format!(
                "no node at {} — is `kapwa serve` running?",
                self.url
            ))
        })?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(body);
        }
        // The node never answers in HTML. A page came from whatever stands
        // in front of it — a tunnel, an edge firewall — and saying so keeps
        // an edge's 403 from reading as a rejected key.
        if body.trim_start().starts_with('<') {
            return Err(Fail::No(format!(
                "{status} from something in front of the node at {}, not the node itself",
                self.url
            )));
        }
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["error"].as_str().map(String::from))
            .unwrap_or(body);
        // 401 is "who are you?"; 403 is the node knowing and saying no
        Err(if status.as_u16() == 401 {
            Fail::Who(msg)
        } else {
            Fail::No(msg)
        })
    }

    async fn get(&self, path: &str) -> Result<String, Fail> {
        self.send(
            self.http.get(format!("{}{path}", self.url)),
            path == "/api/protocol",
        )
        .await
    }

    /// Whatever the environment says this participant is, carried along on
    /// anything it writes. kapwa keeps unknown fields and ignores them, so
    /// this needs no protocol and means nothing here: it is the participant
    /// describing itself, in its own words, for whoever reads the log.
    /// Riding on events that were happening anyway is deliberate — a
    /// heartbeat would be a second kind of traffic and a board full of
    /// "still here".
    async fn event(&self, mut body: Value) -> Result<Value, Fail> {
        if let Some(s) = std::env::var("KAPWA_SELF")
            .ok()
            .filter(|s| !s.trim().is_empty())
        {
            match serde_json::from_str::<Value>(&s) {
                Ok(v) if v.is_object() => {
                    body["self"] = v;
                }
                _ => return Err(Fail::No("KAPWA_SELF must be a JSON object".into())),
            }
        }
        let out = self
            .send(
                self.http
                    .post(format!("{}/api/event", self.url))
                    .json(&body),
                false,
            )
            .await?;
        serde_json::from_str(&out).map_err(|e| Fail::No(e.to_string()))
    }
}

/// `?a=1&b=2`, skipping what was not given.
fn query(pairs: &[(&str, Option<String>)]) -> String {
    let q: Vec<String> = pairs
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={v}")))
        .collect();
    if q.is_empty() {
        String::new()
    } else {
        format!("?{}", q.join("&"))
    }
}

/// A name a person can read at a glance. Hex says nothing: `claude/6a26af`
/// could be anybody. Where a session is working usually says what it is
/// doing, so that leads — `claude/kapwa-a3` — with just enough of the
/// session id after it that two sessions in one place stay two.
fn tag_for(cwd: Option<&str>, session: Option<&str>) -> Option<String> {
    let short: String = session
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(3)
        .collect();
    let place = cwd
        .and_then(|p| p.rsplit('/').find(|s| !s.is_empty()))
        .map(|s| {
            s.chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() {
                        ch.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
        })
        .map(|s| s.trim_matches('-').to_string())
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 20
                && !matches!(
                    s.as_str(),
                    "felixflores" | "home" | "users" | "tmp" | "projects"
                )
        });
    match (place, short.is_empty()) {
        (Some(p), false) => Some(format!("{p}-{short}")),
        (Some(p), true) => Some(p),
        (None, false) => Some(short),
        (None, true) => None,
    }
}

fn scope(a: &Args) -> String {
    if a.t.is_empty() {
        String::new()
    } else {
        format!("?t={}", a.t.join(","))
    }
}

fn history(item: &Value) -> String {
    let mut out = vec![format!(
        "{}  {}\n  {} · {}{}{}",
        item["id"].as_str().unwrap_or(""),
        item["title"].as_str().unwrap_or(""),
        item["status"].as_str().unwrap_or(""),
        match item["owner"].as_str().unwrap_or("") {
            "" => "nobody holds it".to_string(),
            o => format!("held by {o}"),
        },
        match item["asked_of"].as_str().unwrap_or("") {
            "" => String::new(),
            w => format!(" · waiting on {w}"),
        },
        match item["topics"]
            .as_array()
            .map(|t| t.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .filter(|t| !t.is_empty())
        {
            Some(t) => format!(" · #{}", t.join(" #")),
            None => String::new(),
        },
    )];
    for h in item["history"].as_array().into_iter().flatten() {
        let at = h["at"].as_str().unwrap_or("");
        let mut l = format!(
            "  {}  {:<14} {:<5}",
            at.get(5..16).unwrap_or(at).replace('T', " "),
            h["by"].as_str().unwrap_or(""),
            h["verb"].as_str().unwrap_or("")
        );
        if let Some(to) = h["to"].as_str() {
            l += &format!(" → {to}");
        }
        if let Some(t) = h["text"].as_str() {
            l += &format!("  {t}");
        }
        if let Some(f) = h["fold"].as_str() {
            l += &format!("  ({f})");
        }
        out.push(l);
    }
    out.join("\n")
}

/// The hooks `kapwa setup claude` prints. The start hook primes the
/// session with the record. Stop and Notification run `mine --hook`, which
/// is silent unless something addressed the session: that is how a tap from
/// a bot, a person or another harness reaches a session already running.
/// Sessions that can message each other directly do that instead of
/// tapping here, so these carry little between two of them.
const SETUP_CLAUDE: &str = r#"{
  "hooks": {
    "SessionStart": [{
      "matcher": "startup|resume|compact",
      "hooks": [{ "type": "command", "command": "kapwa prime --hook" }]
    }],
    "Stop": [{
      "hooks": [{ "type": "command", "command": "kapwa mine --hook" }]
    }],
    "Notification": [{
      "hooks": [{ "type": "command", "command": "kapwa mine --hook" }]
    }]
  }
}"#;

/// Returns the process exit code.
pub async fn run(raw: Vec<String>) -> i32 {
    let mut a = match parse(&raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("kapwa: {e}\n\n{HELP}");
            return 2;
        }
    };
    // the command is the first bare word, wherever the flags sit; with none,
    // it's the board
    const COMMANDS: [&str; 18] = [
        "board", "say", "take", "drop", "done", "ask", "mine", "show", "prime", "day", "stats",
        "topics", "how", "protocol", "whoami", "setup", "join", "invite",
    ];
    let cmd_owned = match a.pos.first() {
        Some(c) if COMMANDS.contains(&c.as_str()) => a.pos.remove(0),
        Some(c) => {
            eprintln!("kapwa: unknown command `{c}`\n       kapwa --help");
            return 2;
        }
        None => "board".to_string(),
    };
    let cmd = cmd_owned.as_str();
    // a SessionStart hook hands us the session on stdin: make it this
    // session's tag, so many sessions of one key are told apart
    let mut hook_tag = None;
    if cmd == "prime"
        && a.hook
        && a.tag.is_none()
        && std::env::var("KAPWA_TAG").is_err()
        && !std::io::stdin().is_terminal()
    {
        let mut buf = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
        if let Ok(v) = serde_json::from_str::<Value>(&buf) {
            if let Some(tag) = tag_for(v["cwd"].as_str(), v["session_id"].as_str()) {
                a.tag = Some(tag.clone());
                hook_tag = Some(tag);
            }
        }
    }
    let as_json = a.json || !std::io::stdout().is_terminal();
    let node = Node::new(&a);
    let usage = |m: &str| -> i32 {
        eprintln!("kapwa: {m}\n       kapwa --help");
        2
    };
    let mut body = json!({});
    if !a.t.is_empty() {
        body["t"] = json!(a.t);
    }
    if let Some(v) = &a.to {
        body["to"] = json!(v);
    }
    if let Some(v) = &a.p {
        body["p"] = json!(v);
    }

    let result: Result<i32, Fail> = async {
        match (cmd, a.pos.as_slice()) {
            ("board", []) => print!("{}", node.get(&format!("/api/board.txt{}", scope(&a))).await?),
            ("how" | "protocol", []) => print!("{}", node.get("/api/protocol").await?),
            ("whoami", []) => {
                let out = node.get("/api/whoami").await?;
                if as_json {
                    println!("{out}");
                } else {
                    let v: Value = serde_json::from_str(&out).unwrap_or_default();
                    println!("{}", v["name"].as_str().unwrap_or("?"));
                    match &node.tag {
                        Some(t) => println!("  signing as one session of {} · --tag {t}", v["name"].as_str().unwrap_or("").split('/').next().unwrap_or("")),
                        None => println!("  no tag: every session of this key looks like one. --tag <name> to be yourself"),
                    }
                    let topics: Vec<&str> = v["topics"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                    println!("  watching {}", if topics.is_empty() { "everything".to_string() } else { format!("#{}", topics.join(" #")) });
                }
            }
            ("prime", []) => {
                // a session that is already running never sees a tap, because
                // it read its prime at the start and has no reason to look
                // again. --wait is the other way round: ask once, and be told
                let q = if a.wait {
                    let sep = if a.t.is_empty() { "?" } else { "&" };
                    format!("{}{sep}wait=240", scope(&a))
                } else {
                    scope(&a)
                };
                let got = node.get(&format!("/api/prime.txt{q}")).await;
                if a.hook {
                    // a hook must never break the session it starts: say
                    // nothing rather than fail
                    // and with --wait, an empty body is the node saying
                    // nothing arrived: no news is not worth a context window
                    if let Ok(mut text) = got.map(|t| t.trim_end().to_string()) {
                        if text.is_empty() {
                            return Ok(0);
                        }
                        if let Some(tag) = &hook_tag {
                            text += &format!(
                                "\nyou are {tag} here: pass --tag {tag} to every kapwa command, or export KAPWA_TAG={tag} once.\nthe name is yours to choose — anything short and human (--tag dashboard) is better than the default.\n"
                            );
                            // some harnesses let a start hook set the session's environment
                            if let Ok(f) = std::env::var("CLAUDE_ENV_FILE") {
                                use std::io::Write;
                                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(f) {
                                    let _ = writeln!(f, "export KAPWA_TAG={tag}");
                                }
                            }
                        }
                        println!("{}", json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}}));
                    }
                    return Ok(0);
                }
                print!("{}", got?);
            }
            ("topics", []) => print!("{}", node.get("/api/topics").await?),
            ("join", rest) if rest.len() <= 1 => {
                let mut body = json!({});
                if let Some(t) = &a.with {
                    body["invite"] = json!(t);
                } else {
                    // the proof that we are on this machine is a file only
                    // this machine's user can read; if it is not there, we
                    // are not, and an invitation is the way in
                    let p = crate::config::home().join(".config/kapwa/enroll");
                    let Ok(s) = std::fs::read_to_string(&p) else {
                        // the node writes this when it starts, so its absence
                        // means no node runs here — you are somewhere else, and
                        // somewhere else needs somebody to vouch for you
                        return Err(Fail::Who(format!(
                            "no node runs on this machine ({} is not there), so there is nothing here to \
                             prove you belong to. ask someone on the node's machine to run \
                             `kapwa invite <name>`, then `kapwa join --with <invitation> --url <node>`",
                            p.display()
                        )));
                    };
                    body["secret"] = json!(s.trim());
                }
                if let Some(n) = rest.first() {
                    body["name"] = json!(n);
                }
                if let Some(r) = &a.role {
                    body["role"] = json!(r);
                }
                if !a.t.is_empty() {
                    body["t"] = json!(a.t.join(","));
                }
                let out = node.send(node.http.post(format!("{}/api/join", node.url)).json(&body), true).await?;
                let v: Value = serde_json::from_str(&out).map_err(|e| Fail::No(e.to_string()))?;
                let (name, key) = (v["name"].as_str().unwrap_or(""), v["key"].as_str().unwrap_or(""));
                // a key is only ever known once: write it down before saying so
                let path = crate::config::home().join(format!(".config/kapwa/keys/{name}"));
                let saved = std::fs::create_dir_all(path.parent().unwrap())
                    .and_then(|_| std::fs::write(&path, format!("{key}\n")))
                    .and_then(|_| {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                        }
                        Ok(())
                    })
                    .is_ok();
                // a key that is safely on disk must not also go to stdout: JSON
                // is the default whenever output is piped, so printing it means
                // every `kapwa join | tee`, every CI log and every agent
                // transcript keeps a live credential forever. It is emitted
                // only when there was nowhere to put it, and then it must be.
                if as_json {
                    println!(
                        "{}",
                        json!({"name": name, "saved": saved.then(|| path.display().to_string()),
                               "key": (!saved).then(|| key.to_string())})
                    );
                } else if saved {
                    println!("you are {name} · key saved to {}", path.display());
                    println!("  use it with:  kapwa --me {name} whoami   (or export KAPWA_ME={name})");
                } else {
                    println!("you are {name} · key: {key}");
                    println!("  nowhere to save it here — keep it: export KAPWA_KEY={key}");
                }
            }
            ("invite", [name]) => {
                let mut body = json!({"name": name});
                if let Some(r) = &a.role {
                    body["role"] = json!(r);
                }
                if !a.t.is_empty() {
                    body["t"] = json!(a.t.join(","));
                }
                if let Some(h) = a.hours {
                    body["hours"] = json!(h);
                }
                let out = node.send(node.http.post(format!("{}/api/invite", node.url)).json(&body), false).await?;
                let v: Value = serde_json::from_str(&out).map_err(|e| Fail::No(e.to_string()))?;
                if as_json {
                    println!("{out}");
                } else {
                    println!("invitation for {} · {} · watching {} · good until {}",
                        v["name"].as_str().unwrap_or(""), v["role"].as_str().unwrap_or(""),
                        v["topics"].as_str().unwrap_or(""), v["until"].as_str().unwrap_or(""));
                    println!("  one use. give them this line:");
                    println!("    kapwa join --with {} --url {}", v["invite"].as_str().unwrap_or(""), node.url);
                }
            }
            // a day can be named where the command wants it, so
            // `kapwa day yesterday` reads as well as `--on yesterday`
            ("day", rest) | ("stats", rest) if rest.len() < 2 => {
                let (key, given) = match cmd {
                    "day" => ("on", a.on.clone()),
                    _ => ("days", a.days.clone()),
                };
                let q = query(&[
                    (key, given.or_else(|| rest.first().cloned())),
                    ("t", (!a.t.is_empty()).then(|| a.t.join(","))),
                ]);
                let ext = if as_json { "" } else { ".txt" };
                let out = node.get(&format!("/api/{cmd}{ext}{q}")).await?;
                if as_json {
                    println!("{out}");
                } else {
                    print!("{out}");
                }
            }
            ("mine", []) if a.hook => {
                // For a hook that fires often: say nothing at all unless
                // something arrived from somebody else. What you hold is
                // deliberately not a reason to speak — you already know, and a
                // line that prints every time is a line everyone learns to
                // skip, which is the failure this is trying to avoid.
                let v: Value =
                    serde_json::from_str(&node.get(&format!("/api/mine{}", scope(&a))).await?)
                        .unwrap_or_default();
                let n = |k: &str| v[k].as_array().map(Vec::len).unwrap_or(0);
                let (asked, said) = (n("asked"), n("said_to"));
                if asked + said > 0 {
                    let mut parts = vec![];
                    if asked > 0 {
                        parts.push(format!("{asked} asked of you"));
                    }
                    if said > 0 {
                        parts.push(format!("{said} said to you"));
                    }
                    println!("kapwa · {} · `kapwa mine` for what", parts.join(" · "));
                    for i in v["asked"]
                        .as_array()
                        .into_iter()
                        .chain(v["said_to"].as_array())
                        .flatten()
                        .take(3)
                    {
                        println!(
                            "  {} {}",
                            i["id"].as_str().unwrap_or(""),
                            i["title"].as_str().unwrap_or("")
                        );
                    }
                }
            }
            ("mine", []) => {
                let out = node.get(&format!("/api/mine{}", scope(&a))).await?;
                if as_json {
                    println!("{out}");
                } else {
                    print!("{}", node.get(&format!("/api/prime.txt{}", scope(&a))).await?);
                }
            }
            ("show", [id]) => {
                let out = node.get(&format!("/api/item/{id}")).await?;
                if as_json {
                    println!("{out}");
                } else {
                    println!("{}", history(&serde_json::from_str(&out).unwrap_or_default()));
                }
            }
            ("say", [text]) => {
                body["kind"] = json!("say");
                body["text"] = json!(text);
                if let Some(n) = &a.name {
                    body["id"] = json!(n);
                }
                let v = node.event(body).await?;
                done(&v, as_json, "said");
            }
            ("say", [id, text]) => {
                body["kind"] = json!("say");
                body["id"] = json!(id);
                body["text"] = json!(text);
                done(&node.event(body).await?, as_json, "noted");
            }
            ("take", [id]) => {
                body["kind"] = json!("take");
                body["id"] = json!(id);
                let v = node.event(body).await?;
                let me = v["event"]["by"].as_str().unwrap_or("").to_string();
                let full = v["id"].as_str().unwrap_or(id).to_string();
                let mut owner = v["item"]["owner"].as_str().unwrap_or("").to_string();
                if a.wait && owner == me {
                    // one sync interval: long enough for a peer's earlier
                    // take to arrive and win
                    tokio::time::sleep(Duration::from_secs(7)).await;
                    let now: Value = serde_json::from_str(&node.get(&format!("/api/item/{full}")).await?).unwrap_or_default();
                    owner = now["owner"].as_str().unwrap_or("").to_string();
                }
                let won = owner == me;
                if as_json {
                    println!("{}", json!({"id": full, "owner": owner, "yours": won, "provisional": won && !a.wait}));
                } else if won && a.wait {
                    println!("CLAIMED {full}");
                } else if won {
                    println!("took {full} (provisional for a few seconds; --wait to be sure)");
                } else {
                    println!("LOST {full} to {owner}");
                }
                return Ok(if won { 0 } else { 1 });
            }
            ("drop", [id]) | ("done", [id]) => {
                body["kind"] = json!(cmd);
                body["id"] = json!(id);
                done(&node.event(body).await?, as_json, if cmd == "drop" { "dropped" } else { "done" });
            }
            ("done", [id, text]) => {
                body["kind"] = json!("done");
                body["id"] = json!(id);
                body["text"] = json!(text);
                done(&node.event(body).await?, as_json, "done");
            }
            ("ask", [text]) => {
                // about something new: say it, then ask on it
                let made = node.event(json!({"kind":"say","text":text,"t":a.t})).await?;
                body["kind"] = json!("ask");
                body["id"] = made["id"].clone();
                body["text"] = json!(text);
                done(&node.event(body).await?, as_json, "asked");
            }
            ("ask", [id, text]) => {
                body["kind"] = json!("ask");
                body["id"] = json!(id);
                body["text"] = json!(text);
                done(&node.event(body).await?, as_json, "asked");
            }
            ("setup", [what]) if what == "claude" => {
                println!("# add to ~/.claude/settings.json (merge with any hooks you have).\n# it puts `kapwa prime` into every session at start, on resume, and\n# again after compaction, so the board survives long sessions.\n#\n# the other two are for the rest of the session: a tap arrives by pull,\n# so a session already running would not otherwise notice one. they print\n# nothing at all unless somebody asked you something or said something to\n# you, which is why they can afford to run every turn. a tap comes from a\n# bot, a person or an agent that cannot message the session directly;\n# sessions that can, talk with SendMessage and not through kapwa.\n{SETUP_CLAUDE}");
            }
            ("say" | "take" | "drop" | "done" | "ask" | "show" | "setup" | "join" | "invite", _) => return Ok(usage(&format!("`{cmd}` wants different arguments"))),
            (other, _) => return Ok(usage(&format!("unknown command `{other}`"))),
        }
        Ok(0)
    }
    .await;

    match result {
        Ok(code) => code,
        Err(Fail::No(m)) => {
            eprintln!("kapwa: {m}");
            1
        }
        Err(Fail::Who(m)) => {
            eprintln!("kapwa: {m}");
            3
        }
    }
}

fn done(v: &Value, as_json: bool, word: &str) {
    if as_json {
        println!(
            "{}",
            json!({"id": v["id"], "by": v["event"]["by"], "item": v["item"]})
        );
        return;
    }
    let id = v["id"].as_str().unwrap_or("");
    let topics: Vec<&str> = v["item"]["topics"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mine = v["event"]["by"]
        .as_str()
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("");
    let tags = if topics.is_empty() {
        String::new()
    } else {
        format!("  #{}", topics.join(" #"))
    };
    println!("{word} {id}{tags}");
    // it was given nothing to go on, so the node put it where it was safe
    if word == "said" && topics == [mine] {
        println!(
            "  (no topic given, so it went to your own. `kapwa topics` shows what is in use.)"
        );
    }
}
