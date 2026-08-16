# `rio-fake` — Yamaha SCP bring-up / capture harness

A bench tool for recovering the **SCP wire protocol** by making the real
*R Remote* app talk to a fake R-series device. **Not an emulator** — it is a
logger that can be taught to answer.

Pairs with the protocol write-up in
[`docs/yamaha-scp-r-remote.md`](../../docs/yamaha-scp-r-remote.md) and the
session runbook in
[`docs/bench-session-playbook.md`](../../docs/bench-session-playbook.md)
(Phase 3). Sibling of [`tools/dt-fake`](../dt-fake/README.md), which does the
same job for Allen & Heath.

## What it's for

Static analysis of R Remote 6.0.0 recovered the SCP command set, the complete
parameter schema and the 359-command association sequence. It could not recover
two things, because neither is a string in the binary:

1. **The TCP port.**
2. **The framing** of `GET`/`SET` on the wire.

Both fall out of pointing the real app at this tool. **No Rio is required.**

## Run

```
python3 tools/rio-fake/fake_rio.py --iface en0                       # stage 1
python3 tools/rio-fake/fake_rio.py --iface en0 --ports 49280         # stage 2
python3 tools/rio-fake/fake_rio.py --iface en0 --ports 49280 \
    --reply-mode guess --replies myreplies.json --log session.log    # stage 3
```

`--iface` **must** be the machine's real Dante interface — the one DVS and Dante
Controller are bound to. A beacon pinned to a VPN or VM interface is invisible to
the Audinate runtime, which has already cost one session.

## The three stages

**Stage 1 — the port, for free.** The fake does not need to work. It advertises
an R-series-shaped Bonjour record; when R Remote tries to connect, the SYN alone
gives up the port. The tool prints a `*** TCP CONNECT ... SCP PORT = N ***` line
the moment it happens.

If nothing appears, the port is outside `CANDIDATE_PORTS` and we never bound it.
The tool prints a ready-made `tcpdump` line at startup that catches SYNs to ports
it isn't listening on — run that in a second terminal alongside.

**Stage 2 — the framing.** Re-run with `--ports N`. Everything received is
hex-dumped, and because SCP is ASCII, also split into text lines (`>>`). Expect
`DEVINFO` / `SCPMODE` negotiation first.

**Stage 3 — iterate into association.** Turn on `--reply-mode guess`, watch what
the app asks next, refine, repeat. Success looks like R Remote proceeding into
the association sequence — 33 parameter subscriptions expanded to 359 commands
for a Rio — which confirms the sequence and the framing together.

## Replies are guesses

Every entry in `DEFAULT_REPLIES` and every value in `DEV_INFO` is **invented**.
The response framing is exactly what we are trying to learn, so the defaults
exist to be wrong in an informative way. Field *names* and widths come from the
`DevInfo` schema archived at
[`docs/evidence/rremote-600-scp-schemas.xml`](../../docs/evidence/rremote-600-scp-schemas.xml);
the *format* around them does not. **Nothing this tool emits is evidence** — only
what the app sends is.

Override without editing code by passing `--replies FILE`, a JSON array of
`[regex, response]` pairs that takes precedence over the built-in table:

```json
[
  ["^\\s*devinfo\\s+(\\w+)", "devinfo {g1} \"{dev:g1}\""],
  ["^\\s*prmnum", "prmnum 33"]
]
```

Template expansions: `{g1}`…`{gN}` are the regex capture groups, and
`{dev:g1}` looks the first group up in `DEV_INFO` (so one rule serves every
`devinfo` field).

## What it listens on

- **TCP** — `CANDIDATE_PORTS` (49280–49283, 49900) or whatever `--ports` says.
  49280 is the documented Yamaha SCP port for the CL/QL line; whether R-series
  reuses it is **unverified**, and settling that is the point of Stage 1.
- **UDP** — the ConMon ports, mirroring `dt-fake`. R Remote also speaks ConMon
  for Dante-layer management (clocking, naming, firmware), so this catches that
  traffic too.
- **mDNS** — `_netaudio-cmc._udp` and `_netaudio-arc._udp`, pinned via `dns-sd -P`.
  Add more with `--extra-service TYPE:PORT` once Phase 2 of the playbook shows
  what R Remote actually browses for.

## After a session

Keep the `--log` file with the pcap and the action timestamps, and archive both
into `stoatworks-labs/dante-captures` (private). Fold anything learned back into
`docs/yamaha-scp-r-remote.md`, marking what moved from `[OPEN]` to observed.
