# aretheydown

A terminal UI (TUI) that shows the current status of GitHub, AWS, Azure, and other platforms in one screen.

Use it at the start of troubleshooting: if a platform you depend on is having an incident, you know before you start digging into your own infrastructure.

`aretheydown --serve` also shows the same picture as a web page on `http://localhost:7341/`.

> Status: the interactive TUI, `--once` text and JSON output, and the `--serve` web page and JSON server work for GitHub, Cloudflare, AWS, Azure, Google Cloud, OpenAI, Anthropic, and Microsoft 365 (consumer services such as Outlook.com and OneDrive, from its public status page), plus any Statuspage vendor you add in the config file, and HTTP checks of your own endpoints (`[[check]]`).

## Install

Install it as an everyday `aretheydown` command with Homebrew, from the tap [`reidg44/homebrew-tap`](https://github.com/reidg44/homebrew-tap). The formula builds from source, so Homebrew installs Rust for the build.

```bash
brew install reidg44/tap/aretheydown
```

Then run `aretheydown` (or `aretheydown --once`) from anywhere.

Without Homebrew, build it with Cargo (Rust 1.88 or newer):

```bash
cargo install --git https://github.com/reidg44/aretheydown.git --locked
```

**Release a new version:**

1. Bump `version` in `Cargo.toml`, commit, then tag and push: `git tag v0.3.0 && git push origin v0.3.0`.
2. In `homebrew-tap`, set `tag:` and `revision:` (the tag's commit, `git rev-parse v0.3.0`) in `Formula/aretheydown.rb`, then commit and push.
3. On each Mac: `brew update && brew upgrade aretheydown`.

## Setup (fresh machine, to develop)

1. Install Rust (1.88 or newer) and `just`:

   ```bash
   brew install rust just
   ```

2. Install prek and turn on the git hooks (prek downloads betterleaks itself):

   ```bash
   brew install prek
   prek install -f
   ```

3. Optional: copy `.env.example` to `.env` and fill in values. The public status pages need no credentials.

## Run

Open the interactive TUI:

```bash
just run
```

The header gives the answer in one line (`All 7 operational`, or `1 of 7 needs attention: Cloudflare`; a platform that could not be fetched counts as needing attention). The left pane lists every platform, trouble first (the top one is selected when data first loads), with its level, its probe mark (`✓` its front door answered as expected, `✗` it didn't, blank without a probe; see Probes below), and the time it was last updated; the right pane shows the selected platform's probe result and URL on a `Probe:` line, then the selected platform's active incidents (title, affected components, start time with how long ago that was, e.g. `(2h 14m ago)`, the vendor's latest update in full with its stage and age, e.g. `Latest update (monitoring, 3h ago):`, link) or, when it could not be fetched, the error. Below those, `Resolved in the last 24h (N)` lists incidents the vendor resolved in the last 24 hours (Google Cloud and Statuspage vendors only), each with when it was resolved, how long it lasted, and its link; they never change a platform's level. Last, `History (last 7 days)` lists the platform's level changes from the history file `--serve` records (see below), newest first:

```text
History (last 7 days)
  Sat 02:53  Outage → Operational, ongoing
  Sat 02:14  Operational → Outage, lasted 39m
             Elevated error rates on Actions; Degraded Codespaces
  Fri 09:00  Operational, lasted 17h 14m
```

Each entry is a change of level (records that only changed incident text, or a `--serve` restart, don't count), with the incidents active then; the oldest entry is the level in force when the 7 days start, so an outage that began earlier still shows. An entry more than 6 days old adds its date (`Sat Nov 8 02:14`). The TUI only reads the file, again on each refresh; without one the section says `No history: run aretheydown --serve (or brew services start aretheydown) to record it.` (an unreadable file adds why it could not be read) `--history <path>` points the TUI at another file. Up/Down or `j`/`k` select a platform, PageUp/PageDown or `K`/`J` scroll a long incident list (the pane border shows `↑ more`/`↓ more`), Tab and Shift-Tab move a highlight through the selected platform's incidents (it starts on the first and wraps at the ends), `o` opens the highlighted incident in your default browser (or the platform's status page when the incident has no link or there are no incidents; if the browser cannot be opened, the footer says why for a few seconds), `s` switches between worst-first order (the default: Outage, Unknown, Degraded, then Operational) and config order, `t` opens the timeline (below), `r` refreshes now (it also refreshes every 60 seconds), and `q` or Ctrl-C quits (Esc too, except in the timeline). Times are in your local time zone.

`t` swaps the list and detail pane for one full-screen timeline of every platform's level changes over the last 7 days, newest first, so outages on different platforms line up against each other. The border shows how many changes it lists:

```
Mon 16 Nov
  07:15  Cloudflare  Degraded → Operational, ongoing
  06:25  Cloudflare  Operational → Degraded, lasted 50m
                     Workers API errors

Sun 15 Nov
  09:04  GitHub      Outage → Operational, ongoing
```

It reads the same history file the same way as the `History (last 7 days)` section (same entries, rereads on each refresh, same message when there is no history). Up/Down, PageUp/PageDown, and `j`/`k`/`J`/`K` scroll it; `t` or Esc goes back to the list with the same platform selected (Esc doesn't quit while the timeline is open).

Check every platform once and print the result, verdict first, then platforms worst first. Each incident's latest update is on one line, cut to about 200 characters. A platform that resolved incidents in the last 24 hours gets one more line about them:

```bash
just run --once
```

```text
1 of 2 needs attention: Cloudflare
Cloudflare  Degraded     Partially Degraded Service
  probe: responding (HTTP 200 in 293 ms), GET https://www.cloudflare.com/cdn-cgi/trace
  - Cloudflare is seeing an issue with Workers API availability
    affects Workers; since 2026-11-13 15:31 UTC (2h 14m ago)
    monitoring, updated 2h 3m ago: A fix has been implemented and we are monitoring the results.
    https://www.cloudflarestatus.com/incidents/e7ktq32gphhe
  2 resolved in the last 24h, latest: Elevated error rates on R2 buckets (1h 5m ago)
GitHub      Operational  All Systems Operational
  probe: no answer (timed out after 9s), GET https://github.com/
```

Print the same data as JSON (an array with one object per platform, in config order: `name`, `level`, `description`, `incidents`, `resolved`, `fetched_at`, `error`, `probe`: `null` or `url`, `ok`, `code` (`null` without an answer), `elapsed_ms` (likewise), and `description` (`--at --json` always has `null`: history does not keep probes); each incident has `kind` (`incident` or `maintenance`), `title`, `components`, `started_at`, `url`, `age_seconds`: whole seconds from `started_at` to `fetched_at`, or `null` with no start time, `stage`: the vendor's own word such as `investigating` or `monitoring`, or `null` (only Statuspage vendors have one), `ended_at`: `null` for active incidents, and `latest_update`: `null` or `text` (in full), `posted_at`, and `age_seconds` measured the same way. `resolved` holds incidents resolved in the last 24 hours, newest first, with the same fields; their `age_seconds` counts from `ended_at`):

```bash
just run --once --json
```

Keep running and serve a web page plus the same JSON over HTTP, so a browser tab, scripts, other machines, or a status bar can read it without waiting for a fresh fetch:

```bash
just run --serve
open http://localhost:7341/
curl -s localhost:7341/api/status | jq
```

`GET /` is a web page with the same picture as `--once`: the verdict, platforms worst first, each incident with its stage, age, components, full latest update, and link, the incidents resolved in the last 24h, and each platform's status page. It refreshes itself every 60 seconds without reloading (with JavaScript off it still shows, but doesn't refresh), and follows your OS's light or dark mode. Until the first fetch ends it returns `503` with a short page that reloads itself every few seconds.

It fetches every platform on start and then every 60 seconds; requests only read the latest result. `GET /api/status` returns `200` with the same JSON as `--once --json` (a platform in Outage is still a `200`: read `level`), or `503` with `{"error": "first fetch still running"}` until the first fetch ends. `GET /healthz` returns `200 ok`. Ctrl-C or SIGTERM stops it (exit 0). It listens on `127.0.0.1` only unless you pass `--bind`; there is no authentication, so it warns when the address can be reached from the network.

`--serve` also keeps a history of what it saw, so you can look back later ("did that failed deploy line up with an outage?"). Each poll appends a platform's status to the history file only when it changed (level, description, error, or active incidents with their stage and latest update; a new fetch time or resolved list alone is not a change), so the file holds one line per change, not one per poll. The first poll after start writes every platform. The file is JSON Lines, one `{"recorded_at": <RFC 3339>, "status": <platform>}` object per line, where the platform has the same fields as `--json` minus `age_seconds`. It lives at `$XDG_STATE_HOME/aretheydown/history.jsonl`, else `~/.local/state/aretheydown/history.jsonl` (`--history <path>` picks another; the directory is created if missing). Records older than 30 days are dropped each time `--serve` starts. If a write fails, it warns on stderr and keeps serving.

At start, before its first poll is recorded, `--serve` also fills in the time before it first ran from the vendors' own incident history, so `--at` and the TUI's history section can look back further than `--serve` has been running. It reads the same `incidents.json` feeds as the resolved list (Statuspage vendors and Google Cloud; AWS, Azure, and Microsoft 365 publish no history and get none) and writes a record at each past incident's start or end where the platform's level or its active incidents change, the level set by each incident's impact or severity as for live incidents, and `[components]` applied. Only the time before a platform's earliest record in the file and within the last 30 days is filled, so it never lands among records `--serve` wrote and restarting adds nothing. (Once a long-running file's oldest records pass 30 days and are dropped, the next start may fill that freed span from the vendor feed again.) These records carry `"backfilled": true`. Limits: a Statuspage `incidents.json` holds only the page's most recent incidents (often a few weeks' worth), so coverage can be shorter than 30 days; incidents that began before the 30 days show only in their changes after it; and a past incident shows without its stage or latest update (the feed only has the vendor's last word). It does not fill gaps while `--serve` was stopped. A feed that fails or can't be read is a warning naming the platform, and the server starts anyway; it prints `backfilled 257 history records from vendor incident feeds: GitHub, …` when it adds any.

`--serve` can also tell you when a platform's level changes, so you hear about an upstream outage before you start troubleshooting your own systems. Add a webhook to the config file:

```toml
[alerts]
webhook = "https://hooks.slack.com/services/…"
```

`webhook` also takes a list, `webhook = ["https://hooks.slack.com/services/…", "http://127.0.0.1:9000/hook"]`: each alert goes to every URL (a URL listed twice gets it once). To hear about fewer changes, add `levels`:

```toml
[alerts]
webhook = "https://hooks.slack.com/services/…"
levels = ["outage"]
```

With `levels`, only a change into or out of a listed level alerts (spelled like `--json`'s `level`: `operational`, `degraded`, `outage`, `unknown`). With `["outage"]`, Degraded → Outage and Outage → Operational alert; Operational → Degraded and Degraded → Operational do not. Leaving `unknown` out keeps a feed that could not be read (Operational → Unknown) quiet. Without `levels`, every change alerts.

When a platform's level changes, in either direction (Operational → Degraded, Outage → Operational, to or from Unknown), and the new level holds for two polls in a row (about 2 minutes), `--serve` POSTs one JSON alert for that platform:

```json
{
  "text": "GitHub: Operational → Outage. Incident with Git operations (+1 more)",
  "platform": "GitHub",
  "from": "operational",
  "to": "outage",
  "at": "2026-11-15T13:40:00Z",
  "incidents": [{"title": "Incident with Git operations", "url": "https://stspg.io/…"}]
}
```

`text` makes a Slack incoming webhook work as is; other services can read the other fields. `from`/`to` are spelled like `--json`'s `level`, `at` is the fetch that confirmed the change, and `incidents` are the platform's active incidents then (`url` may be `null`). When the new level is `unknown`, `text` ends with the fetch error. A one-poll blip, such as a single failed fetch, sends nothing, and the first poll after start only notes each platform's level. A failed POST (no answer within 10 seconds, or a non-2xx answer) is a warning on stderr naming the platform and the webhook's place in the list (`webhook 2`); that send is dropped, the other webhooks still get theirs, and polling goes on. The webhook URL is a secret (anyone holding it can post), so it is never printed, not even in config errors. The TUI and `--once` ignore `[alerts]`.

Print what every platform looked like at a past moment, from that history:

```bash
just run --at 02:14
just run --at "yesterday 02:14"
just run --at "2h ago"
just run --at "2026-11-14 02:14 UTC" --json
just run --at 2026-11-14T02:14:00Z
```

`--at` takes RFC 3339; `now`; a relative time, number-and-unit groups (`m`, `h`, `d`) then `ago` (`30m ago`, `1d 4h ago`, `1h30m ago`); or a clock time in local time: `HH:MM`, `today HH:MM`, `yesterday HH:MM`, or `YYYY-MM-DD HH:MM`. A bare `HH:MM` is the most recent one: at 09:00, `--at 23:00` means yesterday 23:00 (`today 23:00` means today). End a clock time in ` UTC` or `Z` (`yesterday 02:14 UTC`, `02:14Z`) to read it, and its `today`/`yesterday`, in UTC. Outside RFC 3339, case and extra spaces don't matter. For each platform it shows the last record at or before that time, in the same text as `--once` under a first line `At 2026-11-14 02:14 (local), from history recorded by --serve:`, or the same JSON with `--json`. Each status is shown as it was when recorded: ages count from when it was fetched, and its `resolved in the last 24h` line is the one it had then (a later resolution that did not change the level shows only in a later record). It only sees what `--serve` recorded or backfilled: a platform with neither is not shown, a platform taken from a backfilled record says so after its description (`1 active incident (from vendor history)`; `"backfilled": true` in `--json`), and a gap while `--serve` was not running shows the last status recorded before it. Exit codes match `--once` (0 all Operational, 1 otherwise); 2 means a bad time, a missing history file, or nothing recorded by then (the message gives the earliest record). Unreadable lines in the file are skipped with a warning.

| Flag              | What it does                                                    |
| ----------------- | --------------------------------------------------------------- |
| `--once`          | Fetch every platform once, print, and exit                      |
| `--json`          | With `--once` or `--at`, print JSON instead of text             |
| `--serve`         | Poll every 60s, serve a web page at `http://127.0.0.1:7341/` and the JSON at `/api/status`, record changes to the history file, and alert the `[alerts]` webhooks |
| `--at <time>`     | Print what each platform looked like at `<time>`, from `--serve`'s history |
| `--history <path>` | The history file `--serve` writes and `--at` and the TUI read (default `~/.local/state/aretheydown/history.jsonl`; not with `--once`) |
| `--port <port>`   | With `--serve`, listen on this port (default 7341)              |
| `--bind <addr>`   | With `--serve`, listen on this address (default `127.0.0.1`; `0.0.0.0` opens it to the network, no auth) |
| `--config <path>` | Use this config file instead of the default (see below)         |

### Run in the background

With the Homebrew install, `brew services` keeps `aretheydown --serve` running as a launchd user agent: it starts at login, restarts if it exits, and serves the page at `http://127.0.0.1:7341/`, records history, and sends alerts as above.

```bash
brew services start aretheydown
curl -s http://127.0.0.1:7341/healthz
brew services list
```

- Stop it with `brew services stop aretheydown` (it gets SIGTERM and exits 0).
- After changing the config file, run `brew services restart aretheydown`: the config is read only at start.
- Its startup lines and warnings go to `$(brew --prefix)/var/log/aretheydown.log`.
- It runs with no options, so the default paths apply: the config at `~/.config/aretheydown/config.toml` and the history at `~/.local/state/aretheydown/history.jsonl`. `aretheydown --at …` and the TUI's history section read that same history.

### Probes

Next to what each vendor says, aretheydown shows what it saw itself: every refresh, one GET of the vendor's own service (its front door, not its status page), without credentials and without following redirects. "Vendor says Operational, probe: no answer" points at trouble the vendor hasn't posted yet, or at your own network. A probe never changes a platform's level, the verdict, the sort, the exit code, history, or alerts.

Most front doors answer a request without credentials with a redirect or an error, so each probe has an expected status: an exact code (`401`) or a class (`4xx`). The defaults (all answered as listed on 2026-11-16):

| Platform      | Probe URL                                   | Expected |
| ------------- | ------------------------------------------- | -------- |
| GitHub        | `https://github.com/`                       | 2xx      |
| Cloudflare    | `https://www.cloudflare.com/cdn-cgi/trace`  | 2xx      |
| AWS           | `https://s3.amazonaws.com/`                 | 3xx      |
| Azure         | `https://management.azure.com/`             | 4xx      |
| Google Cloud  | `https://storage.googleapis.com/`           | 4xx      |
| OpenAI        | `https://api.openai.com/v1/models`          | 401      |
| Anthropic     | `https://api.anthropic.com/v1/models`       | 401      |
| Microsoft 365 | `https://outlook.office.com/`               | 3xx      |

The result is `responding (HTTP 401 in 220 ms)`, `unexpected HTTP 503 (expected 4xx)`, or `no answer (timed out after 9s)` (also `no answer (could not connect)` and `no answer (the request failed)`). It runs alongside the status fetch and gives up after 9 seconds, so it never delays the platform past its 10-second limit. When the status feed fails, the probe still shows. It appears in the TUI (the `✓`/`✗` column and the `Probe:` line), in `--once` (a `probe:` line under the platform), in `--json` and `/api/status` (`probe`), and on the `--serve` web page (a `Probe:` line under the platform's description). Change, turn off, or add probes with `[probes]` in the config file (below).

### Config file (optional)

Add more vendors that use Atlassian Statuspage, check your own endpoints, hide built-in platforms, limit AWS to the regions you use, watch only some components of a Statuspage platform, or give `--serve` webhooks to alert (see `--serve` above), in `~/.config/aretheydown/config.toml` (or `$XDG_CONFIG_HOME/aretheydown/config.toml`; the same path on every OS). Without a config file, only the built-in platforms are checked. See the commented example in [`config.example.toml`](config.example.toml):

```toml
hide = ["Cloudflare"]

[[statuspage]]
name = "Vercel"
url = "https://www.vercel-status.com"

[[check]]
name = "API"
url = "https://api.example.com/healthz"
# expect = 200   # optional; default: any 2xx

[aws]
regions = ["us-east-1", "us-west-2"]

[components]
GitHub = ["Actions", "Packages"]

[probes]
OpenAI = false                                      # no probe
GitHub = { url = "https://api.github.com/", expect = "2xx" }
Vercel = { url = "https://vercel.com/", expect = "3xx" }
```

OpenAI and Anthropic are built in. If your config file still adds either one under `[[statuspage]]`, delete that block: a duplicate name is a config error (exit code 2).

`[[check]]` adds one of your own endpoints as a platform, after the vendors, so the first screen answers "is it me or them?". Each check is a plain GET (redirects followed): the expected status (`expect`, default any 2xx) is `operational` with `Responding: HTTP 200 in 142 ms`; any other status is `outage` with `HTTP 503 (expected 2xx)`, and so is no connection (`could not connect`) or no answer within 9 seconds (`timed out after 9s`). A check is never `unknown`. Since redirects are followed, a 3xx `expect` only matches a redirect without a `Location` to follow. History records a check when its level or status code changes, not each time its response time does. It shows everywhere a vendor does (TUI, `--once`, `--json`, the `--serve` page, history, alerts), counts in the verdict and exit code, and `o` opens its URL. The URL is shown on screen and in `--json`, so don't put a secret in it.

`[aws] regions` makes AWS count only events in those region codes, so a long-running event in a region you don't use doesn't keep AWS at `outage`. Global events (not tied to a region) always count, and the AWS line says how many events in other regions it left out. Without it, every region counts.

`[components]` does the same for Statuspage platforms (GitHub, Cloudflare, OpenAI, Anthropic, and any `[[statuspage]]` vendor): key = platform name, value = component or group names as the status page shows them (neither is case-sensitive; a group name such as `"North America"` covers every component in it). With a list, the platform's level comes from those components' status and the incidents touching them, not the vendor's overall status, so Cloudflare datacenter maintenance doesn't turn it yellow if you only use Workers. Incidents that list no components always count. The line reads like `1 of 1 watched components not operational: Workers (1 incident on other components not shown)`, and ends with `(not in feed: "…")` for a listed name the feed doesn't have (vendors rename components). Without an entry, the platform shows the vendor's overall status.

`[probes]` changes the probes described under Probes above. Key = platform name (not case-sensitive); value = `false` to turn its probe off, or a table with `url` (http or https; redirects are not followed), `expect` (a code such as `401`, or a class `"1xx"` to `"5xx"`, any case), or both. A `url` without `expect` expects any 2xx; `expect` alone keeps the built-in URL. A `[[statuspage]]` vendor has no probe until you give it one with a `url`. A `[[check]]` can't have one (it already is a probe of your endpoint). The URL is shown on screen and in `--json`, so don't put a secret in it.

An invalid config (bad TOML, an unknown key, an unknown name in `hide`, a duplicate name, a URL that is not http or https, a check `expect` outside 100 to 599, an empty or malformed `aws.regions`, or a `[components]` entry that is empty, is listed twice, or names an unknown, hidden, or non-Statuspage platform, a `[probes]` entry that names an unknown or hidden platform or a check, has a URL that is not http(s), an `expect` that isn't 100 to 599 or `1xx` to `5xx`, or no `url` for a platform without a built-in probe, or an `[alerts]` table without a `webhook`, with an empty `webhook` list or a URL in it that is not an http(s) URL with a host, or with an empty `levels` list or an unknown level in it) stops the app with exit code 2 and an error that names the file and the problem. A `--config` file that does not exist is also an error.

Levels: `operational`, `degraded`, `outage`, `unknown`. A platform that fails or takes longer than 10 seconds shows as `unknown`, with the reason in `error` (a `[[check]]` shows `outage` instead, see above). Scheduled maintenance that is in progress counts as `degraded` and is listed as `[maintenance]`; upcoming maintenance is not shown.

Exit codes, so scripts can gate on upstream health:

| Code | Meaning                                              |
| ---- | ---------------------------------------------------- |
| 0    | Every platform is Operational                        |
| 1    | At least one platform is Degraded, Outage, or Unknown |
| 2    | Could not run: usage or setup error (the TUI also exits 2 if it cannot use the terminal; it exits 0 when you quit) |

## Develop

| Command      | What it does                                   |
| ------------ | ---------------------------------------------- |
| `just build` | Debug build                                    |
| `just run`   | Run the app                                    |
| `just test`  | Run all tests                                  |
| `just lint`  | `cargo fmt --check` + clippy, warnings as errors |
| `just fmt`   | Format the code                                |
| `just check` | Lint + test (run before committing)            |

CI (`.github/workflows/ci.yml`) runs `just check`, a Rust 1.88 build, and the prek hooks on every pull request and push to `main`.
