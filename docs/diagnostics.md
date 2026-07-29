# Diagnostics

Three artefacts, because a failure on site needs different things at different
moments: a log an operator can read now, a crash report that survives a failure
nobody was watching, and one file that can be sent afterwards.

The module is **vendored**, not shared: ``crates/diag``. Every repo in the fleet
carries its own identical copy, so no repo depends on another to build. Fix a
bug here and it needs applying to the others — that is the accepted trade.

## Where things are written

| Platform | Directory |
| --- | --- |
| macOS | `~/Library/Logs/preamp-bridge/` |
| Linux | `$XDG_STATE_HOME/preamp-bridge/logs/` (default `~/.local/state/...`) |
| Windows | `%LOCALAPPDATA%\preamp-bridge\logs\` |

`BABELBOX_LOG_DIR` overrides it. The path is printed on the first line of every
run, so nobody has to remember the table.

Logs go under *state*, not cache: a cache directory may be cleared at any time,
and the point of a crash report is to outlive the crash.

## 1. The human log

`preamp-bridge.YYYY-MM-DD.log`, rotated daily, seven kept, no colour escapes.

Verbosity comes from `BABELBOX_LOG` (falling back to `RUST_LOG`), default `info`.

Console output goes to **stderr**. Anything on stdout is program output —
`--collect-diagnostics` prints a path there and nothing else, so it can be used
in a script.

## 2. The crash report

Written by a panic hook, before unwinding finishes, to
`preamp-bridge-crash-<timestamp>.json`; the path is printed on the way down. The
backtrace uses `force_capture`, so it does not depend on the operator having
set `RUST_BACKTRACE` before the thing that crashed crashed — which they never
have.

It carries the app version and git revision (`-dirty` means uncommitted changes
were built — a version number alone cannot tell a released build from one three
commits past the tag), the platform, the process, the effective configuration
with secret-looking keys replaced, the fault itself, and the last 500 log lines
from an in-memory ring.

The ring matters: the log file writer is asynchronous, so at the moment of a
fault the lines that explain it may not have reached disk yet.

## 3. The diagnostics bundle

```bash
preamp-bridge --collect-diagnostics
```

Writes `preamp-bridge-diagnostics-<timestamp>.json` and prints its path. One file, so
"send me your diagnostics" is one instruction rather than a conversation about
which of six files were wanted. It holds the identity and config blocks, the
last three log files (tail-capped at 5000 lines), the five most recent crash
reports embedded whole, and `collection_warnings` for anything unreadable —
collection is best-effort, because a missing log file must not stop the rest
being sent.

## Redaction

Keys matching `password`, `passwd`, `passphrase`, `secret`, `token`, `apikey`,
`credential`, `auth` or `private` — case-insensitive, `-`/`_` ignored — are
replaced at any depth, including inside arrays. Deliberately over-eager: a
redacted port number costs nothing, a token in a file forwarded to a mailing
list costs a great deal.

## Schema

Both documents carry `"schema": "stoatworks.diagnostics/1"` and a `kind` of
`crash-report` or `diagnostics-bundle` — the same contract in every repo and
every language, so one parser reads all of them. Treat the schema string as the
contract; bump it if a field changes meaning.

## Trying it

A crash handler that has never been fired is a guess, not a feature:

```bash
cargo run -p diag --example crash
```
