# Working in kapwa

Read [`README.md`](README.md) and [`PROTOCOL.md`](PROTOCOL.md) first. The design
reasoning, and every open decision, is in [`ideas/`](ideas/)
(https://kapwa-ideas.felixflor.es).

## What kapwa is

What participants owe each other: a shared, append-only record of who has
promised what to whom. People, agents and whole collectives are the same kind of
participant. Coordination happens by leaving marks, not by sending messages.

## Rules

1. **One thing.** kapwa is a ledger of commitments. What a collective *knows* is
   kita's. Moving bytes and real-time connections are another tool's. The test for
   any feature: is it about what we know, what we owe, or what we move? Only
   "owe" belongs here.

2. **kapwa knows no sibling.** Other protocols ride it through two generic slots,
   `["ref", uri, word?]` and `["payload", type, string]`. Never put another
   project's name into the protocol, the tags or the code.

3. **It is a protocol, not an app.** If it doesn't fit on the index card it is a
   convention, and nobody has to learn it. A second implementation in another
   language must stay an afternoon's work. Test vectors are the contract.

4. **Keep it small.** Four verbs and `ask`. Unknown kinds and fields are kept, and
   ignored. No version negotiation, ever.

5. **A topic, or your own.** A new item with no topic is filed under its
   author's name. Nothing is global by accident, and `kapwa topics` exists so
   an open vocabulary does not fill with synonyms: look before inventing a word.

6. **Every writer owns one log.** Nobody writes anyone else's. Sync is by `seq`
   and two-way over one outbound connection, so a mesh needs one address.
   "Gateway" is a property (always on, reachable), never a role.

7. **No domain nouns.** No person's name, product or business term in a status,
   a field, a function or an example.

8. **Fail closed.** Nothing is open but `/healthz` and the sign-in flow.

9. **Measure, do not assert.** A claim about cost or scale comes with the script
   that produced it.

10. **Micro-commits.** One decision, one commit; the message carries what, why,
   what was rejected. `diwa search kapwa "<topic>"` before non-trivial work.

## Layout

```
src/config.rs   env → Config (reads ~/.config/kapwa/env itself)
src/log.rs      per-writer logs, own seq, mirror ingest (contiguous only)
src/board.rs    the fold
src/puller.rs   one task per peer under a restart-on-panic supervisor
src/auth.rs     who is asking: mesh token | agent key (+ session tag) | sign-in
src/cli.rs      the client half of the one binary; holds no rules of its own
src/oidc.rs     Pocket ID sign-in
src/render.rs   prime, board, the manual, and the read-only page
src/routes.rs   the HTTP surface
scripts/        deploy.sh · two-node.sh and gateway.sh (the failure cases, runnable)
ideas/          the design site, static HTML
```

## Commands

```
cargo test && cargo clippy --all-targets -- -D warnings
A_PORT=3510 B_PORT=3511 scripts/two-node.sh    # beside a live node
scripts/deploy.sh <ssh-host> [--intel]
```

`v0.1.0` (tagged) was a different program under this name: ssh-only, no daemon,
with remote exec and skill sync. Those are not the ledger's job and are not here.
