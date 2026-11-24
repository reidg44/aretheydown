# STATUS

## Resume here

v0.3.0 is the first public release, renamed from `platform-status`. The backlog is empty. Next: run `generate-ideas` for new work. Check the app first:

```bash
just check && just run --once
```

## What works

All eight built-in platforms (GitHub, Cloudflare, AWS, Azure, Google Cloud, OpenAI, Anthropic, Microsoft 365 consumer services) in four ways:

- **TUI** (`just run`): a verdict line, worst platform first, incidents with age, stage, and latest update, and the incidents resolved in the last 24h. Each row has a probe mark (`✓`/`✗`) and the detail pane a `Probe:` line. A failed refresh keeps the last good data for 10 minutes (`stale 4m`). Tab/`o` open incidents, and URLs are OSC 8 links. The detail pane ends with `History (last 7 days)` from the history file, and `t` shows every platform's changes in one full-screen timeline.
- **`--once` / `--json`**: the same picture as text or JSON, with a `probe:` line / `probe` object per platform.
- **`--serve`**: polls every 60s and serves a web page at `http://127.0.0.1:7341/` plus `/api/status`; the page and `/api/status` show each probe. It records changes to a JSON Lines history file (30 days kept, no probes) and POSTs `[alerts]` webhooks on held level changes (`levels` filter, several URLs). At start it backfills the history before its first record from the Statuspage and Google Cloud incident feeds (`backfilled: true`). `brew services start aretheydown` runs it in the background.
- **`--at <time>`**: what every platform looked like then, from that history. Platforms from vendor history are marked `(from vendor history)`.

Vendor probes: one credential-free GET of each vendor's front door per refresh, shown beside its level and never changing it (docs/probes.md has the default table). A config file can add Statuspage vendors and `[[check]]` endpoints, hide built-ins, limit AWS regions, limit Statuspage components, and change probes with `[probes]` (`config.example.toml`). Install: `brew install reidg44/tap/aretheydown` (README → Install; releasing is in docs/development.md). CI runs `just check`, an MSRV build, the prek hooks, and betterleaks.

Level mappings per feed: `AGENTS.md` → "Status sources".

## Open tickets

None.

## Known limits

- Azure counts every feed item as Degraded, including ones titled "Mitigated …" that stay up after a fix. The feed has never shown one; revisit if one causes a false alarm.
- History backfill covers only Statuspage vendors and Google Cloud, and only as far back as their `incidents.json` reaches (often a few weeks). It shows no stage or update for past incidents and doesn't fill gaps while `--serve` was stopped (docs/history.md).
- Probe defaults were checked live on 2026-11-16. If a vendor changes what its front door answers, the probe shows `✗` with `unexpected HTTP …`; fix the default in `probe::DEFAULTS` or override it with `[probes]`.

Suggested skills: `generate-ideas`, then `next-ticket`.
