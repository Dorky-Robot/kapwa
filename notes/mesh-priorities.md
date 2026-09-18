# Three things the mesh was owed

Board items `a1c11`, `ad652`, `a688c`. What changed, where the gate is, and
how to see it working against a live node on `:3410`.

Set `K` to a key on that node, and `U` to the node:

    U=http://127.0.0.1:3410
    K=$(grep '^kapwa:' ~/.config/kapwa/agents | cut -d: -f2)

## a1c11 · a topic that is nobody's business by default

A key with no topic list means "everything", so clinical items imported on
2026-09-17 were in the first screen of every session that started. They came
in as an ordinary run of `say` carrying `t: [clinical-records, everyday,
port]` from one key — there is no importer to fix, which is why the fence
has a write side as well as a read one.

| gate | where |
| --- | --- |
| `Lens { topics, fence }`, and `fenced()` | `src/board.rs:127`, `src/board.rs:166` |
| which patterns stay shut for a caller | `src/routes.rs:633` (`fence`) |
| narrow scope · wide scope | `src/routes.rs:644` (`lens`), `src/routes.rs:654` (`wide`) |
| the write side | `src/routes.rs:490` |
| `KAPWA_PRIVATE_TOPICS` | `src/config.rs:39`, `src/config.rs:131` |
| test | `src/routes.rs:1572` |

Every surface that carries item text goes through the lens, because each one
was a way around a filter applied only to the board: `prime`, `board`, `mine`
(`src/render.rs:399`), the feed (`src/render.rs:113`), the raw fold and an
item by id (`src/routes.rs:815`, `src/routes.rs:828`), the day, the stats,
and the sandbox pages.

**Why it is configuration and not a list of words in the source.** CLAUDE.md
rule 7 keeps domain nouns out of fields, functions and examples, and the
words asked for are domain nouns. A fence whose vocabulary lives in the
deployment is also one a second implementation does not have to hardcode.
The cost is real and worth naming: **a node that sets nothing is fenced by
nothing** and behaves exactly as before. Setting it is a deploy step, not a
default.

Add to `~/.config/kapwa/env` on every node that syncs with this one, and
restart:

    KAPWA_PRIVATE_TOPICS=everyday,clinical-records,port,phi,patient,clinical

Patterns match as **substrings**, so `clinical` alone covers
`clinical-records` and anything clinical invented later. Then:

    curl -s -H "Authorization: Bearer $K" $U/api/prime.txt | grep -ci prescriptions   # 0
    curl -s -H "Authorization: Bearer $K" "$U/api/board.txt?t=everyday" | grep -ci soap  # 0, asking does not help
    curl -s -H "Authorization: Bearer $K" -H 'content-type: application/json' \
         -d '{"kind":"say","text":"x","t":["everyday"]}' $U/api/event                  # 403

To give one key the topic back, its line in `~/.config/kapwa/agents` names it:

    name:token:role:everyday,clinical-records

**It is a lens, not a redaction.** Fenced events still replicate to every
node and are still in the logs on disk. This keeps records off shared
default boards; it is not a way to un-write them. If the goal is that they
never leave the machine they were written on, that is a different change and
a bigger one.

## ad652 · a person who can write

`/api/event` took `Kind::Agent` only, so `ask --to felix` was a dead letter:
11 asks aged ~20h, 0 replies, 0 events ever written by a person.

| gate | where |
| --- | --- |
| what a person may write | `src/routes.rs:420` (`A_PERSON_MAY`), `src/routes.rs:433` |
| drop only what you hold | `src/routes.rs:475` |
| CSRF, header or form field | `src/auth.rs:118`, `src/auth.rs:175`, `src/auth.rs:188` |
| the token, back to its own session | `src/routes.rs:585` (`whoami`) |
| the one form on the page | `src/routes.rs:536` (`answer`), `src/render.rs:730` |
| tests | `src/routes.rs:1413`, `src/routes.rs:1531`, `src/routes.rs:1486` |

`say`, `take`, `done`, and `drop` of what you hold. Not `ask` — the one
participant who cannot be automated is also the one whose queue everything
lands in. Not `invite` or `rotate`, which is where a stolen session would
actually cost something.

**A person now writes under their Pocket ID username, not their email**
(`src/auth.rs:137`). Two reasons: an ask addressed to `felix` would never
have matched `felix@…`, and `by` goes into an append-only log that everyone
on the mesh reads and nobody can edit. **Check that the username in Pocket ID
is the name the asks use** — if it is not, answering will write under the
wrong name, and that cannot be taken back.

Sessions signed in before this have no token and cannot write until they
sign in again.

    curl -s -b cookies.txt $U/api/whoami                 # {... "csrf": "…"}
    curl -s -b cookies.txt -H "X-Kapwa-CSRF: $CSRF" -H 'content-type: application/json' \
         -d '{"kind":"say","id":"<ask>","text":"B, because …"}' $U/api/event
    curl -s -b cookies.txt -d 'kind=ask' $U/api/event    # 403, no token and not a person's verb

From a browser: sign in, and the ask that names you has a box under it.

## a688c · a tap a running session notices

SessionStart primes once. A tap that lands during a session is delivered by
pull like everything else, so nobody sees it until the next session.

| gate | where |
| --- | --- |
| `?wait=N` long-poll | `src/routes.rs:706` |
| `kapwa prime --wait` | `src/cli.rs:432` |
| `kapwa mine --hook` | `src/cli.rs:574` |
| hooks `kapwa setup claude` prints | `src/cli.rs:339` |
| test | `src/routes.rs:1454` |

Two ways in, both pull, neither writing anything:

    curl -s -H "Authorization: Bearer $K" "$U/api/prime.txt?wait=30"   # blocks; 204 if nothing
    kapwa prime --wait                                                # the same, from a terminal

and hooks, for a session that cannot block:

    kapwa setup claude        # SessionStart + Stop + Notification

`Stop` is the one that matters — it fires at the end of every turn, so a
long session checks often. All three print **nothing at all** unless
something arrived, so a quiet board costs no context.

It speaks for `asked of you` and `said to you` only, never for what you
hold. You already know what you hold, and a line that prints every turn is a
line everybody learns to skip — which is the failure this is trying to
avoid, not a smaller version of it.

No Slack, no broadcast on write, nothing pushed to any agent.

## Not done

- **`kapwa setup claude` is printed, not installed.** Merging it into
  `~/.claude/settings.json` is still by hand, on each machine.
- **No LaunchAgent was deployed and no key was moved.** Every node needs
  `KAPWA_PRIVATE_TOPICS` set and a restart before its fence exists.
- **`/api/log/:writer` is unfenced** and must stay so: replication reads raw
  logs, and a node that filtered what it handed a peer would break sync.
  This is the same point as "a lens, not a redaction".
