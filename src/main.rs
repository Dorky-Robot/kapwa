mod config;
mod discovery;
mod identity;
mod inbox;
mod skills;
mod ssh;

use anyhow::Result;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str());

    match cmd {
        None | Some("identity") => cli_identity(&args[1..]),
        Some("peers") => cli_peers(&args[2..]),
        Some("ask") => cli_ask(&args[2..]),
        Some("tell") => cli_tell(&args[2..]),
        Some("run") => cli_run(&args[2..]),
        Some("inbox") => cli_inbox(&args[2..]),
        Some("skills") => cli_skills(&args[2..]),
        Some("skill") => cli_skill(&args[2..]),
        Some("--version" | "-V") => {
            println!("kapwa {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("help" | "--help" | "-h") => {
            print_help();
            Ok(())
        }
        Some(other) => {
            eprintln!("Unknown command: {}", other);
            eprintln!("Run `kapwa help` for usage.");
            std::process::exit(1);
        }
    }
}

// ── identity ──────────────────────────────────────────────────────────

fn cli_identity(args: &[String]) -> Result<()> {
    let config = config::Config::load()?;
    let name_only = args.iter().any(|a| a == "--name-only");

    if name_only {
        println!("{}", config.identity);
        return Ok(());
    }

    let json = identity::gather_json(&config.identity)?;
    println!("{}", json);
    Ok(())
}

// ── peers ─────────────────────────────────────────────────────────────

fn cli_peers(args: &[String]) -> Result<()> {
    let config = config::Config::load()?;
    let use_cache = args.iter().any(|a| a == "--cached");
    let json_flag = args.iter().any(|a| a == "--json");

    let peers = if use_cache {
        discovery::cached()
    } else {
        eprintln!("Scanning SSH config for kapwa peers...");
        discovery::scan(&config.identity, 5)
    };

    if json_flag {
        println!("{}", serde_json::to_string_pretty(&peers)?);
        return Ok(());
    }

    if peers.is_empty() {
        println!("No kapwa peers found.");
        println!("Install kapwa on other machines in your ~/.ssh/config to discover them.");
        return Ok(());
    }

    println!(
        "{:<20} {:<20} {:<24}",
        "NAME", "SSH", "LAST SEEN"
    );
    println!("{}", "\u{2500}".repeat(64));

    for peer in &peers {
        println!(
            "{:<20} {:<20} {:<24}",
            peer.name, peer.ssh, peer.last_seen,
        );
    }
    Ok(())
}

// ── ask ───────────────────────────────────────────────────────────────

fn cli_ask(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        eprintln!("Usage: kapwa ask <peer> <query>");
        eprintln!("Queries: identity, tunnels, routes [tunnel], updates");
        std::process::exit(1);
    }

    let config = config::Config::load()?;
    let peer_name = &args[0];
    let query = &args[1];

    let peer = resolve_peer(peer_name, &config.identity)?;
    let timeout = 10;

    match query.as_str() {
        "identity" => {
            let result = ssh::kapwa_cmd(&peer, "identity", timeout)?;
            if result.success {
                print!("{}", result.stdout);
            } else {
                eprintln!("Failed: {}", result.stderr.trim());
                std::process::exit(1);
            }
        }
        "tunnels" => {
            let result = ssh::tunnels_cmd(&peer, "list --json", timeout)?;
            if result.success {
                print!("{}", result.stdout);
            } else {
                eprintln!("Failed: {}", result.stderr.trim());
                std::process::exit(1);
            }
        }
        "routes" => {
            let tunnel = args.get(2).map(|s| s.as_str()).unwrap_or("");
            let cmd = if tunnel.is_empty() {
                "routes --json".to_string()
            } else {
                format!("routes {} --json", tunnel)
            };
            let result = ssh::tunnels_cmd(&peer, &cmd, timeout)?;
            if result.success {
                print!("{}", result.stdout);
            } else {
                eprintln!("Failed: {}", result.stderr.trim());
                std::process::exit(1);
            }
        }
        "updates" => {
            let result = ssh::exec(&peer, "brew outdated 2>/dev/null", timeout)?;
            if result.stdout.trim().is_empty() {
                println!("No pending updates on {}.", peer_name);
            } else {
                println!("Pending updates on {}:", peer_name);
                print!("{}", result.stdout);
            }
        }
        other => {
            eprintln!("Unknown query: {}", other);
            eprintln!("Available: identity, tunnels, routes, updates");
            std::process::exit(1);
        }
    }

    Ok(())
}

// ── tell ──────────────────────────────────────────────────────────────

fn cli_tell(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        eprintln!("Usage: kapwa tell <peer> \"<message>\"");
        std::process::exit(1);
    }

    let config = config::Config::load()?;
    let peer_name = &args[0];
    let message = args[1..].join(" ");

    let peer = resolve_peer(peer_name, &config.identity)?;

    let msg_json = serde_json::json!({
        "from": config.identity,
        "body": message,
    });

    let escaped = msg_json.to_string().replace('\'', "'\\''");
    let cmd = format!(
        "if command -v kapwa >/dev/null 2>&1; then kapwa inbox append '{}'; \
         elif [ -x ~/.local/bin/kapwa ]; then ~/.local/bin/kapwa inbox append '{}'; \
         else echo 'kapwa not found' >&2; exit 1; fi",
        escaped, escaped
    );
    let result = ssh::exec(&peer, &cmd, 10)?;

    if result.success {
        println!("Message sent to {}.", peer_name);
    } else {
        eprintln!("Could not deliver to {}: {}", peer_name, result.stderr.trim());
        std::process::exit(1);
    }

    Ok(())
}

// ── inbox ─────────────────────────────────────────────────────────────

fn cli_inbox(args: &[String]) -> Result<()> {
    match args.first().map(|s| s.as_str()) {
        None => {
            let messages = inbox::read()?;
            print!("{}", inbox::format_messages(&messages));
            Ok(())
        }
        Some("clear") => {
            inbox::clear()?;
            println!("Inbox cleared.");
            Ok(())
        }
        Some("append") => {
            // Internal: kapwa inbox append '{"from":"...", "body":"..."}'
            let json = args.get(1)
                .ok_or_else(|| anyhow::anyhow!("expected JSON argument"))?;
            let msg: serde_json::Value = serde_json::from_str(json)?;
            let from = msg["from"].as_str().unwrap_or("unknown");
            let body = msg["body"].as_str().unwrap_or("");
            inbox::append(from, body)?;
            Ok(())
        }
        Some(other) => {
            eprintln!("Unknown inbox command: {}", other);
            std::process::exit(1);
        }
    }
}

// ── run ───────────────────────────────────────────────────────────────

fn cli_run(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        eprintln!("Usage: kapwa run <peer> \"<command>\"");
        std::process::exit(1);
    }

    let config = config::Config::load()?;
    let peer_name = &args[0];
    let command = args[1..].join(" ");

    let peer = resolve_peer(peer_name, &config.identity)?;
    let result = ssh::exec(&peer, &command, 30)?;

    if !result.stdout.is_empty() {
        print!("{}", result.stdout);
    }
    if !result.stderr.is_empty() {
        eprint!("{}", result.stderr);
    }

    if !result.success {
        std::process::exit(1);
    }

    Ok(())
}

// ── skills ────────────────────────────────────────────────────────────

fn cli_skills(args: &[String]) -> Result<()> {
    let json_flag = args.iter().any(|a| a == "--json");
    let names = skills::list();

    if json_flag {
        println!("{}", serde_json::to_string(&names)?);
        return Ok(());
    }

    if names.is_empty() {
        println!("No skills installed.");
        println!("Skills are markdown files in ~/.config/kapwa/skills/");
        return Ok(());
    }

    for name in &names {
        println!("  {}", name);
    }

    Ok(())
}

fn cli_skill(args: &[String]) -> Result<()> {
    if args.is_empty() {
        eprintln!("Usage: kapwa skill <show|sync> ...");
        std::process::exit(1);
    }
    match args[0].as_str() {
        "show" => {
            let name = args.get(1)
                .ok_or_else(|| anyhow::anyhow!("Usage: kapwa skill show <name>"))?;
            let content = skills::show(name)?;
            print!("{}", content);
            Ok(())
        }
        "sync" => {
            let config = config::Config::load()?;
            let peers = discovery::cached();
            if peers.is_empty() {
                println!("No known peers. Run `kapwa peers` first to discover them.");
                return Ok(());
            }
            for peer in &peers {
                print!("Syncing skills from {}... ", peer.name);
                match skills::sync_from_peer(peer, 10) {
                    Ok(report) => {
                        if report.pulled.is_empty() {
                            println!("up to date.");
                        } else {
                            println!("pulled: {}", report.pulled.join(", "));
                        }
                    }
                    Err(e) => println!("failed: {}", e),
                }
            }
            Ok(())
        }
        other => {
            eprintln!("Unknown skill command: {}", other);
            std::process::exit(1);
        }
    }
}

// ── help ──────────────────────────────────────────────────────────────

fn print_help() {
    println!("kapwa — machines as mutual selves");
    println!();
    println!("Coordination infrastructure for an agentic mesh. Peers are");
    println!("discovered automatically from your ~/.ssh/config — any machine");
    println!("with kapwa installed is a peer. No manual registration needed.");
    println!();
    println!("USAGE:");
    println!("  kapwa                              Show this machine's identity");
    println!("  kapwa identity [--name-only]        Identity (or just the name)");
    println!();
    println!("DISCOVERY:");
    println!("  kapwa peers                         Scan and list kapwa peers");
    println!("  kapwa peers --cached                Show last scan results (fast)");
    println!();
    println!("COMMUNICATION:");
    println!("  kapwa ask <peer> <query>             Query a peer's state");
    println!("    Queries: identity, tunnels, routes [tunnel], updates");
    println!("  kapwa tell <peer> \"<message>\"        Send message to peer");
    println!("  kapwa inbox                          Read incoming messages");
    println!("  kapwa inbox clear                    Clear inbox");
    println!();
    println!("EXECUTION:");
    println!("  kapwa run <peer> \"<command>\"         Execute command on peer");
    println!();
    println!("SKILLS:");
    println!("  kapwa skills                         List installed skills");
    println!("  kapwa skill show <name>              Print a skill");
    println!("  kapwa skill sync                     Pull skills from peers");
    println!();
    println!("HOW DISCOVERY WORKS:");
    println!("  kapwa reads your ~/.ssh/config, probes each Host entry with");
    println!("  `kapwa identity --name-only`, and any machine that responds");
    println!("  is a peer. Results are cached in ~/.config/kapwa/peers.json.");
    println!();
    println!("EXAMPLES:");
    println!("  kapwa peers                          Discover who's out there");
    println!("  kapwa ask mini identity              Full state of mini");
    println!("  kapwa run mac2019 \"tunnels restart og\"");
    println!("  kapwa tell mini \"I need to reboot for updates\"");
    println!("  kapwa skill show drain-and-reboot");
}

// ── helpers ───────────────────────────────────────────────────────────

/// Resolve a peer name to a Peer struct. Checks cache first, scans if needed.
fn resolve_peer(name: &str, my_identity: &str) -> Result<discovery::Peer> {
    discovery::find_peer(name, my_identity)
        .ok_or_else(|| anyhow::anyhow!(
            "peer '{}' not found. Run `kapwa peers` to discover peers.", name
        ))
}
