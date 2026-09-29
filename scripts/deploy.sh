#!/usr/bin/env bash
# Build the kapwa binary here, ship it to a mesh machine, restart its
# LaunchAgent with `launchctl kickstart -k` (a reload only when the plist
# changes, and then detached so a dropped ssh cannot strand it). One static
# file per arch; nothing to install on the target.
#
#   scripts/deploy.sh <ssh-host>                        # upgrade an existing node
#   scripts/deploy.sh <ssh-host> --intel                # x86_64 target (the 2019)
#   scripts/deploy.sh <ssh-host> --writer mini \
#       --peers https://kapwa-mac2024.felixflor.es,... \
#       --public-url https://kapwa-mini.felixflor.es    # first deploy: also writes env
#
# The env file is written once and never overwritten; OIDC fields start
# empty and are filled by hand from the network's Pocket ID. The mesh token
# is copied from this machine's own ~/.config/kapwa/env so every node shares it.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

HOST=${1:?ssh host}; shift
BUILD=1 WRITER="" PEERS="" PUBLIC="" TARGET=""
while [ $# -gt 0 ]; do
  case $1 in
    --no-build) BUILD=0 ;;
    --intel) TARGET=x86_64-apple-darwin ;;
    --writer) WRITER=$2; shift ;;
    --peers) PEERS=$2; shift ;;
    --public-url) PUBLIC=$2; shift ;;
    *) echo "unknown arg $1"; exit 1 ;;
  esac
  shift
done

LABEL=com.dorkyrobot.kapwa
if [ -n "$TARGET" ]; then BIN=target/$TARGET/release/kapwa; else BIN=target/release/kapwa; fi

if [ "$BUILD" = 1 ]; then
  echo "→ building ${TARGET:-native} release"
  cargo build --release --quiet ${TARGET:+--target $TARGET}
fi
[ -x "$BIN" ] || { echo "no $BIN"; exit 1; }

# first-deploy env, composed locally so no secret rides in argv
ENV_TMP=""
if [ -n "$WRITER" ]; then
  MESH_TOKEN=$(sed -n 's/^KAPWA_MESH_TOKEN=//p' "$HOME/.config/kapwa/env" 2>/dev/null | head -1 || true)
  [ -n "$MESH_TOKEN" ] || { echo "no KAPWA_MESH_TOKEN in ~/.config/kapwa/env here — set it first so nodes share it"; exit 1; }
  ENV_TMP=$(mktemp); chmod 600 "$ENV_TMP"
  cat >"$ENV_TMP" <<EOF
KAPWA_WRITER=$WRITER
KAPWA_PORT=3410
KAPWA_PEERS=$PEERS
KAPWA_PUBLIC_URL=$PUBLIC
KAPWA_MESH_TOKEN=$MESH_TOKEN
KAPWA_SECRET_KEY_BASE=$(head -c 64 /dev/urandom | base64)
# dashboard sign-in: Pocket ID → settings → OIDC clients → callback $PUBLIC/auth/callback
KAPWA_OIDC_ISSUER=
KAPWA_OIDC_CLIENT_ID=
KAPWA_OIDC_CLIENT_SECRET=
EOF
fi

echo "→ shipping to $HOST"
ssh "$HOST" 'mkdir -p ~/.config/kapwa ~/.local/kapwa/releases ~/Library/Logs/kapwa'
scp -q "$BIN" "$HOST:/tmp/kapwa.new"
if [ -n "$ENV_TMP" ]; then
  scp -q "$ENV_TMP" "$HOST:~/.config/kapwa/env.new"
  rm -f "$ENV_TMP"
fi

ssh "$HOST" LABEL="$LABEL" 'bash -s' <<'REMOTE'
set -euo pipefail
ROOT=$HOME/.local/kapwa
NEW=$ROOT/releases/$(date +%Y%m%d%H%M%S)
mkdir -p "$NEW"
mv /tmp/kapwa.new "$NEW/kapwa" && chmod +x "$NEW/kapwa"
ln -sfn "$NEW" "$ROOT/current"
# one name to find: the client is the same binary, on PATH
mkdir -p "$HOME/.local/bin" && ln -sfn "$ROOT/current/kapwa" "$HOME/.local/bin/kapwa"

ENV=$HOME/.config/kapwa/env
if [ -f "$ENV.new" ]; then
  if [ -f "$ENV" ]; then rm -f "$ENV.new"; echo "  env exists; keeping it"
  else mv "$ENV.new" "$ENV"; chmod 600 "$ENV"; echo "  wrote $ENV (OIDC fields empty)"; fi
fi
[ -f "$ENV" ] || { echo "no $ENV — first deploy needs --writer/--peers/--public-url"; exit 1; }
[ -f "$HOME/.config/kapwa/agents" ] || { (umask 077; echo "# name:token:role:products" >"$HOME/.config/kapwa/agents"); }

# the binary reads ~/.config/kapwa/env itself; launchd just runs it
PLIST=$HOME/Library/LaunchAgents/$LABEL.plist
WANT=$(mktemp)
cat >"$WANT" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>$ROOT/current/kapwa</string><string>serve</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$HOME/Library/Logs/kapwa/launchd.log</string>
  <key>StandardErrorPath</key><string>$HOME/Library/Logs/kapwa/launchd.log</string>
</dict></plist>
EOF

# Restart without ever leaving the job unloaded behind a dead session.
# The plist runs $ROOT/current/kapwa, so a new release needs only a
# restart: kickstart -k is one call, and launchd does both halves itself.
# bootout + bootstrap is only for a first install or a changed plist, and
# then both halves run detached, so an ssh that drops in between cannot
# leave the job booted out with nothing to bring it back (2026-09-20).
U=$(id -u)
# (|| true: not loaded is an answer here, and pipefail would make it fatal)
pid_now() { { launchctl print "gui/$U/$LABEL" 2>/dev/null | awk '$1 == "pid" { print $3; exit }'; } || true; }
OLD=$(pid_now)
if launchctl print "gui/$U/$LABEL" >/dev/null 2>&1; then
  if [ -f "$PLIST" ] && cmp -s "$WANT" "$PLIST"; then
    rm -f "$WANT"
    echo "  restarting: kickstart -k"
    launchctl kickstart -k "gui/$U/$LABEL"
  else
    mv "$WANT" "$PLIST"
    echo "  plist changed: reloading, detached"
    nohup sh -c "launchctl bootout 'gui/$U/$LABEL'; sleep 2; launchctl bootstrap 'gui/$U' '$PLIST' || { sleep 3; launchctl bootstrap 'gui/$U' '$PLIST'; }" </dev/null >/dev/null 2>&1 &
  fi
else
  # not loaded, so there is nothing to cut: one call, no bootout
  mv "$WANT" "$PLIST"
  echo "  first install: bootstrap"
  launchctl bootstrap "gui/$U" "$PLIST" || { sleep 3; launchctl bootstrap "gui/$U" "$PLIST"; }
fi

# up means a new process answering, not the old one before it went down
for _ in $(seq 60); do
  sleep 0.5
  NOW=$(pid_now)
  [ -n "$NOW" ] && [ "$NOW" != "$OLD" ] || continue
  if out=$(curl -sf http://127.0.0.1:3410/healthz); then echo "  up (pid $NOW): $out"; exit 0; fi
done
echo "  node did not come up; log tail:"; tail -30 "$HOME/Library/Logs/kapwa/launchd.log"; exit 1
REMOTE

# keep the last three releases for rollback
ssh "$HOST" 'ls -dt ~/.local/kapwa/releases/* 2>/dev/null | tail -n +4 | xargs rm -rf 2>/dev/null || true'
echo "✓ $HOST"
