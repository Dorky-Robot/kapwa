# kapwa

Agentic mesh coordination for machines that care for each other. Each machine runs kapwa to provide identity, discovery, communication, remote execution, and shared skills. The intelligence comes from AI agents (Claude Code) — kapwa is the nervous system, not the brain.

## Architecture

- **config.rs** — Minimal config: identity name + peer list. `~/.config/kapwa/config.json`
- **identity.rs** — Dynamic machine state: hostname, arch, tunnels, services, disk, load, pending updates. Shells out to `tunnels list --json`, `brew outdated --json`, `sw_vers`, `df`, `sysctl`
- **ssh.rs** — SSH command execution with timeout and BatchMode=yes. The transport layer for all peer communication
- **inbox.rs** — Simple message store at `~/.config/kapwa/inbox.json`. Append/read/clear
- **skills.rs** — Skill files at `~/.config/kapwa/skills/*.md`. List/show/sync between peers
- **main.rs** — CLI dispatch (hand-rolled, no clap)

## Build & Install

```
cargo build --release
cp target/release/kapwa ~/.local/bin/
```

## CLI

```
kapwa                              # Show own identity (default)
kapwa identity                     # Full JSON state of this machine
kapwa peers                        # List configured peers
kapwa ask <peer> <query>           # Query peer (identity|tunnels|routes|updates)
kapwa tell <peer> "<message>"      # Send message to peer's inbox
kapwa inbox                        # Read messages
kapwa inbox clear                  # Clear inbox
kapwa run <peer> "<command>"       # Execute command on peer via SSH
kapwa skills                       # List installed skills
kapwa skill show <name>            # Print a skill
kapwa skill sync                   # Pull skills from peers
kapwa peer add <name> --ssh <alias>
kapwa peer rm <name>
kapwa --version
kapwa help
```

## Design Principles

- **No daemon, no hardcoded logic.** Kapwa provides primitives. AI agents provide intelligence.
- **Skills are markdown, not code.** New capabilities = new `.md` files, not new Rust.
- **SSH is the only transport.** Already configured between all machines. No HTTP servers, no ports to open.
- **Shells out to `tunnels`.** Never imports tunnels code. SRP preserved.
- **Minimal config.** Identity + peers. Everything else is discovered dynamically.
