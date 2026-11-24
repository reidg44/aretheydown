# aretheydown

aretheydown shows the current health of GitHub, AWS, Azure, and other platforms on one screen in your terminal.

Run it when you start troubleshooting. If a platform you depend on has an incident, you know before you dig into your own systems.

It checks these platforms out of the box:

- GitHub, Cloudflare, AWS, Azure, and Google Cloud
- OpenAI and Anthropic
- Microsoft 365 consumer services, such as Outlook.com and OneDrive (from Microsoft's public status page)

You can also add any vendor that uses Atlassian Statuspage, and HTTP checks of your own endpoints. See [Config file](#config-file-optional).

## Install

Install it with Homebrew from the tap [`reidg44/homebrew-tap`](https://github.com/reidg44/homebrew-tap):

```bash
brew install reidg44/tap/aretheydown
```

The formula builds from source, so Homebrew also installs Rust for the build. Then run `aretheydown` (or `aretheydown --once`) from any folder.

If you don't use Homebrew, build it with Cargo. You need Rust 1.88 or newer:

```bash
cargo install --git https://github.com/reidg44/aretheydown.git --locked
```

## Ways to run it

| Command                 | What you get                                                        |
| ----------------------- | ------------------------------------------------------------------- |
| `aretheydown`           | [The interactive screen](#the-interactive-screen), refreshed every 60 seconds |
| `aretheydown --once`    | [One check, printed as text or JSON](#check-once-and-print), then exit |
| `aretheydown --serve`   | [A web page and JSON API](#run-as-a-web-server), plus history and alerts |
| `aretheydown --at 02:14` | [What every platform looked like at a past time](#look-back-in-time) |

From a checkout of this repo, use `just run` with the same options (`just run --once`).

## The interactive screen

```bash
just run
```

### What it shows

The header gives the answer in one line, such as `All 7 operational` or `1 of 7 needs attention: Cloudflare`. A platform that could not be fetched counts as needing attention.

The left pane lists every platform, with trouble first. When data first loads, the top platform is selected. Each row shows:

- the platform's level
- its probe mark: `✓` if its front door answered as expected, `✗` if it didn't, and blank if it has no probe (see [Probes](#probes))
- the time it was last updated

The right pane shows the selected platform, in this order:

1. A `Probe:` line with the probe's result and URL.
2. Its active incidents. Each one has a title, the affected components, the start time and how long ago that was (`(2h 14m ago)`), the vendor's latest update in full with its stage and age (`Latest update (monitoring, 3h ago):`), and a link. If the platform could not be fetched, you see the error instead.
3. `Resolved in the last 24h (N)`: incidents the vendor resolved in the last 24 hours, each with when it was resolved, how long it lasted, and its link. Only Google Cloud and Statuspage vendors have this list. Resolved incidents never change a platform's level.
4. `History (last 7 days)`: the platform's level changes, newest first, from the history file that `--serve` records (see [History file](#history-file)).

Times are in your local time zone.

### History section

```text
History (last 7 days)
  Sat 02:53  Outage → Operational, ongoing
  Sat 02:14  Operational → Outage, lasted 39m
             Elevated error rates on Actions; Degraded Codespaces
  Fri 09:00  Operational, lasted 17h 14m
```

Each entry is a change of level, with the incidents that were active then. A record that only changed incident text doesn't count, and neither does a `--serve` restart. The oldest entry is the level in force when the 7 days start, so an outage that began earlier still shows. An entry more than 6 days old also shows its date (`Sat Nov 8 02:14`).

The screen only reads the history file. It reads it again on each refresh. To read a different file, pass `--history <path>`. If there is no file, the section says `No history: run aretheydown --serve (or brew services start aretheydown) to record it.` If the file can't be read, the message also says why.

### Timeline

Press `t` to swap the list and detail pane for one full-screen timeline. It shows every platform's level changes over the last 7 days, newest first, so you can line up outages on different platforms. The border shows how many changes it lists.

```
Mon 16 Nov
  07:15  Cloudflare  Degraded → Operational, ongoing
  06:25  Cloudflare  Operational → Degraded, lasted 50m
                     Workers API errors

Sun 15 Nov
  09:04  GitHub      Outage → Operational, ongoing
```

The timeline reads the same history file as the `History (last 7 days)` section. It shows the same entries, rereads the file on each refresh, and shows the same message when there is no history.

### Keys

| Key                          | What it does |
| ---------------------------- | ------------ |
| Up/Down or `j`/`k`           | Select a platform (in the timeline: scroll) |
| PageUp/PageDown or `K`/`J`   | Scroll a long incident list. The pane border shows `↑ more` or `↓ more` when there is more to see. |
| Tab and Shift-Tab            | Move a highlight through the selected platform's incidents. It starts on the first and wraps at the ends. |
| `o`                          | Open the highlighted incident in your default browser. If the incident has no link, or there are no incidents, it opens the platform's status page. If the browser can't be opened, the footer says why for a few seconds. |
| `s`                          | Switch between worst-first order (the default: Outage, Unknown, Degraded, then Operational) and config order |
| `t`                          | Open or close the timeline |
| `r`                          | Refresh now (it also refreshes every 60 seconds) |
| `q` or Ctrl-C                | Quit |
| Esc                          | Quit. In the timeline, Esc goes back to the list instead. |

When you leave the timeline, the same platform is still selected.

## Check once and print

```bash
just run --once
```

This checks every platform once, prints the result, and exits. The output starts with the verdict, then lists platforms worst first. Each incident's latest update fits on one line, cut to about 200 characters. A platform that resolved incidents in the last 24 hours gets one more line about them.

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

Scripts can use the [exit code](#exit-codes): 0 means every platform is Operational.

### JSON output

```bash
just run --once --json
```

This prints the same data as a JSON array, with one object per platform, in config order. Unlike the text, JSON keeps each update's text whole.

Each platform object has these fields:

| Field         | What it holds |
| ------------- | ------------- |
| `name`        | The platform's name |
| `level`       | `operational`, `degraded`, `outage`, or `unknown` |
| `description` | The one-line summary |
| `incidents`   | Active incidents (fields below) |
| `resolved`    | Incidents resolved in the last 24 hours, newest first, with the same fields |
| `fetched_at`  | When the status was fetched |
| `error`       | Why the fetch failed, or `null` |
| `probe`       | `null`, or an object with `url`, `ok`, `code`, `elapsed_ms`, and `description`. `code` and `elapsed_ms` are `null` when there was no answer. With `--at --json`, `probe` is always `null`, because history does not keep probes. |

Each incident has these fields:

| Field           | What it holds |
| --------------- | ------------- |
| `kind`          | `incident` or `maintenance` |
| `title`         | The incident's title |
| `components`    | The affected components |
| `started_at`    | When it started |
| `url`           | Its link |
| `age_seconds`   | Whole seconds from `started_at` to `fetched_at`, or `null` if there is no start time. For `resolved` entries, it counts from `ended_at`. |
| `stage`         | The vendor's own word, such as `investigating` or `monitoring`, or `null`. Only Statuspage vendors have one. |
| `ended_at`      | `null` for active incidents |
| `latest_update` | `null`, or an object with `text` (in full), `posted_at`, and `age_seconds` (measured the same way) |

## Run as a web server

```bash
just run --serve
open http://localhost:7341/
curl -s localhost:7341/api/status | jq
```

`--serve` keeps running and serves the latest result over HTTP. A browser tab, scripts, other machines, or a status bar can read it without waiting for a new fetch. It fetches every platform on start and then every 60 seconds. Requests only read the latest result.

| Address           | What it returns |
| ----------------- | --------------- |
| `GET /`           | A web page with the same picture as `--once` |
| `GET /api/status` | `200` with the same JSON as `--once --json` |
| `GET /healthz`    | `200 ok` |

The web page shows the verdict and the platforms, worst first. For each incident it shows the stage, age, components, full latest update, and link. It also shows the incidents resolved in the last 24 hours and each platform's status page. The page refreshes itself every 60 seconds without reloading. With JavaScript off, it still shows but doesn't refresh. It follows your OS's light or dark mode.

Until the first fetch ends:

- `GET /` returns `503` with a short page that reloads itself every few seconds.
- `GET /api/status` returns `503` with `{"error": "first fetch still running"}`.

The HTTP status never reports platform health. A platform in Outage still returns `200`, so read `level`.

To stop the server, press Ctrl-C or send SIGTERM. It exits with code 0.

The server listens on `127.0.0.1` only, unless you pass `--bind`. It has no authentication, so it warns you when the address can be reached from the network.

### History file

`--serve` keeps a history of what it saw, so you can look back later. For example: "did that failed deploy line up with an outage?"

- **What it writes.** Each poll adds a platform's status to the file only when the status changed. A change is a new level, description, error, or set of active incidents (including their stage and latest update). A new fetch time or a new resolved list alone is not a change. So the file holds one line per change, not one per poll. The first poll after each start writes every platform.
- **Format.** JSON Lines: one `{"recorded_at": <RFC 3339>, "status": <platform>}` object per line. The platform has the same fields as `--json`, except `age_seconds` and `probe`.
- **Where it lives.** `$XDG_STATE_HOME/aretheydown/history.jsonl`, or `~/.local/state/aretheydown/history.jsonl` if `XDG_STATE_HOME` isn't set. Use `--history <path>` to pick another file. The folder is created if it's missing.
- **How long it keeps records.** Each time `--serve` starts, it drops records older than 30 days.
- **If a write fails,** it prints a warning and keeps serving.

### Filling in history from vendor feeds

When `--serve` starts, before it records its first poll, it fills in the time before it first ran. It uses the vendors' own incident history. This lets `--at` and the screen's history section look back further than `--serve` has been running.

How it works:

- It reads the same `incidents.json` feeds as the resolved list. Only Statuspage vendors and Google Cloud have them. AWS, Azure, and Microsoft 365 publish no history, so they get none.
- It writes a record at each past incident's start or end where the platform's level or its set of active incidents changes. Each incident's impact or severity sets the level, the same way as for live incidents. `[components]` applies too.
- It fills only the time before a platform's earliest record in the file, and only within the last 30 days. So it never adds records among the ones `--serve` wrote, and restarting adds nothing. One exception: once a long-running file drops its records older than 30 days, the next start may fill that freed time from the vendor feed again.
- These records carry `"backfilled": true`.
- When it adds any records, it prints a line such as `backfilled 257 history records from vendor incident feeds: GitHub, …`.
- If a feed fails or can't be read, it prints a warning that names the platform and starts anyway.

Limits:

- A Statuspage `incidents.json` holds only the page's most recent incidents, often a few weeks' worth. So coverage can be shorter than 30 days.
- An incident that began before the 30 days shows only in its changes after that point.
- A past incident shows without its stage or latest update, because the feed only has the vendor's last word.
- It does not fill gaps from times when `--serve` was stopped.

### Alerts

`--serve` can tell you when a platform's level changes, so you hear about an upstream outage before you start troubleshooting your own systems. Add a webhook to the [config file](#config-file-optional):

```toml
[alerts]
webhook = "https://hooks.slack.com/services/…"
```

`webhook` also takes a list, such as `webhook = ["https://hooks.slack.com/services/…", "http://127.0.0.1:9000/hook"]`. Each alert goes to every URL. A URL listed twice gets each alert once.

**When it sends an alert.** A platform's level must change, in either direction (for example Operational → Degraded, Outage → Operational, or to or from Unknown). The new level must also hold for two polls in a row, about 2 minutes. Then `--serve` POSTs one JSON alert for that platform. A one-poll blip, such as a single failed fetch, sends nothing. The first poll after start only notes each platform's level.

**To hear about fewer changes,** add `levels`:

```toml
[alerts]
webhook = "https://hooks.slack.com/services/…"
levels = ["outage"]
```

With `levels`, only a change into or out of a listed level sends an alert. Spell levels the same way as `--json`'s `level`: `operational`, `degraded`, `outage`, `unknown`. For example, with `["outage"]`:

- Degraded → Outage and Outage → Operational send an alert.
- Operational → Degraded and Degraded → Operational do not.

If you leave out `unknown`, a feed that could not be read (Operational → Unknown) stays quiet. Without `levels`, every change sends an alert.

**What the alert looks like:**

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

- `text` lets a Slack incoming webhook work as is. Other services can read the other fields.
- `from` and `to` are spelled like `--json`'s `level`.
- `at` is the time of the fetch that confirmed the change.
- `incidents` lists the platform's active incidents at that time. `url` may be `null`.
- When the new level is `unknown`, `text` ends with the fetch error.

**If a POST fails** (no answer within 10 seconds, or an answer that isn't 2xx), it prints a warning. The warning names the platform and the webhook's place in the list (`webhook 2`). That alert is dropped, not retried. The other webhooks still get theirs, and polling goes on.

**Keep the webhook URL secret.** Anyone who has it can post to it. aretheydown never prints it, not even in config errors.

The interactive screen and `--once` ignore `[alerts]`.

## Look back in time

```bash
just run --at 02:14
just run --at "yesterday 02:14"
just run --at "2h ago"
just run --at "2026-11-14 02:14 UTC" --json
just run --at 2026-11-14T02:14:00Z
```

`--at` prints what every platform looked like at a past time, from the history file. For each platform, it shows the last record at or before that time. The output is the same text as `--once`, under a first line such as `At 2026-11-14 02:14 (local), from history recorded by --serve:`. Add `--json` for the same JSON as `--once --json`.

`--at` accepts these times:

| Form                 | Example                               |
| -------------------- | ------------------------------------- |
| RFC 3339             | `2026-11-14T02:14:00Z`                |
| Now                  | `now`                                 |
| Relative             | `30m ago`, `1d 4h ago`, `1h30m ago` (number-and-unit groups using `m`, `h`, or `d`, then `ago`) |
| Local clock time     | `HH:MM`, `today HH:MM`, `yesterday HH:MM`, or `YYYY-MM-DD HH:MM` |

A bare `HH:MM` means the most recent one. At 09:00, `--at 23:00` means yesterday at 23:00, but `today 23:00` means today. To read a clock time in UTC, end it with ` UTC` or `Z` (`yesterday 02:14 UTC`, `02:14Z`). `today` and `yesterday` are then UTC days too. Except for RFC 3339, case and extra spaces don't matter.

Each status shows as it was when it was recorded. Ages count from when it was fetched. Its `resolved in the last 24h` line is the one it had then. A later resolution that didn't change the level shows only in a later record.

`--at` only sees what `--serve` recorded or filled in from vendor feeds:

- A platform with no record is not shown.
- A platform taken from a filled-in record says so after its description: `1 active incident (from vendor history)`. With `--json`, it has `"backfilled": true`.
- During a time when `--serve` was not running, a platform shows the last status recorded before it stopped.

Exit codes match `--once`: 0 if all are Operational, 1 otherwise. Exit code 2 means a bad time, a missing history file, or nothing recorded by that time. In the last case, the message gives the earliest record. Lines in the file that can't be read are skipped with a warning.

## All options

| Option             | What it does |
| ------------------ | ------------ |
| `--once`           | Fetch every platform once, print, and exit |
| `--json`           | With `--once` or `--at`, print JSON instead of text |
| `--serve`          | Poll every 60 seconds, serve a web page at `http://127.0.0.1:7341/` and the JSON at `/api/status`, record changes to the history file, and send `[alerts]` webhooks |
| `--at <time>`      | Print what each platform looked like at `<time>`, from `--serve`'s history |
| `--history <path>` | The history file that `--serve` writes and that `--at` and the interactive screen read. The default is `~/.local/state/aretheydown/history.jsonl`. You can't use it with `--once`. |
| `--port <port>`    | With `--serve`, listen on this port (default 7341) |
| `--bind <addr>`    | With `--serve`, listen on this address (default `127.0.0.1`). `0.0.0.0` opens it to the network, with no authentication. |
| `--config <path>`  | Use this config file instead of the default (see [Config file](#config-file-optional)) |

## Run in the background

If you installed with Homebrew, `brew services` keeps `aretheydown --serve` running as a launchd user agent. It starts when you log in and restarts if it exits. It serves the page at `http://127.0.0.1:7341/`, records history, and sends alerts, as described above.

```bash
brew services start aretheydown
curl -s http://127.0.0.1:7341/healthz
brew services list
```

- To stop it, run `brew services stop aretheydown`. It gets SIGTERM and exits with code 0.
- After you change the config file, run `brew services restart aretheydown`. It reads the config only when it starts.
- Its startup lines and warnings go to `$(brew --prefix)/var/log/aretheydown.log`.
- It runs with no options, so it uses the default paths: the config at `~/.config/aretheydown/config.toml` and the history at `~/.local/state/aretheydown/history.jsonl`. `aretheydown --at …` and the interactive screen's history section read that same history.

## Probes

A probe shows what aretheydown saw itself, next to what the vendor says. On every refresh, it sends one GET to the vendor's own service (its front door, not its status page). It sends no credentials and doesn't follow redirects.

If the vendor says Operational but the probe gets no answer, the vendor may have trouble it hasn't posted yet, or your own network may have trouble.

A probe never changes a platform's level, the verdict, the sort order, the exit code, history, or alerts.

Most front doors answer a request without credentials with a redirect or an error. So each probe has an expected status: an exact code (`401`) or a class (`4xx`). These are the defaults. All of them answered as listed on 2026-11-16.

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

The result is one of:

- `responding (HTTP 401 in 220 ms)`
- `unexpected HTTP 503 (expected 4xx)`
- `no answer (timed out after 9s)`, `no answer (could not connect)`, or `no answer (the request failed)`

The probe runs alongside the status fetch and gives up after 9 seconds, so it never holds a platform past its 10-second limit. When the status feed fails, the probe result still shows.

You see probes in these places:

- the interactive screen: the `✓`/`✗` column and the `Probe:` line
- `--once`: a `probe:` line under the platform
- `--json` and `/api/status`: the `probe` field
- the `--serve` web page: a `Probe:` line under the platform's description

To change, turn off, or add probes, use `[probes]` in the [config file](#config-file-optional).

## Config file (optional)

Without a config file, aretheydown checks only the built-in platforms. With one, you can:

- add vendors that use Atlassian Statuspage (`[[statuspage]]`)
- check your own endpoints (`[[check]]`)
- hide built-in platforms (`hide`)
- count only the AWS regions you use (`[aws] regions`)
- watch only some components of a Statuspage platform (`[components]`)
- change or turn off probes (`[probes]`)
- send `--serve` alerts to webhooks (`[alerts]`, see [Alerts](#alerts))

The file lives at `~/.config/aretheydown/config.toml`, or `$XDG_CONFIG_HOME/aretheydown/config.toml` if `XDG_CONFIG_HOME` is set. The path is the same on every OS. [`config.example.toml`](config.example.toml) has a commented example. Here is a short one:

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

OpenAI and Anthropic are built in. If your config file still adds either one under `[[statuspage]]`, delete that block. A duplicate name is a config error (exit code 2).

### Your own endpoints: `[[check]]`

A `[[check]]` adds one of your own endpoints as a platform, listed after the vendors. Then the first screen answers "is it me or them?"

Each check is a plain GET, and it follows redirects:

- The expected status is `operational`, with a description such as `Responding: HTTP 200 in 142 ms`. The expected status is `expect` if you set it, or any 2xx if you don't.
- Any other status is `outage`, such as `HTTP 503 (expected 2xx)`.
- No connection (`could not connect`) or no answer within 9 seconds (`timed out after 9s`) is also `outage`.

A check is never `unknown`. Because it follows redirects, a 3xx `expect` only matches a redirect that has no `Location` to follow.

History records a check when its level or status code changes, not each time its response time changes. A check shows everywhere a vendor does (the interactive screen, `--once`, `--json`, the `--serve` page, history, and alerts). It counts in the verdict and exit code, and `o` opens its URL.

The URL shows on screen and in `--json`, so don't put a secret in it.

### AWS regions: `[aws] regions`

`[aws] regions` makes AWS count only events in those region codes. Then a long-running event in a region you don't use doesn't keep AWS at `outage`. Global events (not tied to a region) always count. The AWS line says how many events in other regions it left out. Without `regions`, every region counts.

### Statuspage components: `[components]`

`[components]` does the same for Statuspage platforms: GitHub, Cloudflare, OpenAI, Anthropic, and any `[[statuspage]]` vendor.

- The key is the platform name.
- The value is a list of component or group names, as the status page shows them. A group name, such as `"North America"`, covers every component in that group.
- Neither key nor names are case-sensitive.

With a list, the platform's level comes from those components' status and the incidents that touch them, not from the vendor's overall status. For example, if you only use Workers, Cloudflare datacenter maintenance doesn't turn Cloudflare yellow. Incidents that list no components always count.

The platform's line reads like `1 of 1 watched components not operational: Workers (1 incident on other components not shown)`. If a name you listed isn't in the feed, the line ends with `(not in feed: "…")`. This can happen when a vendor renames a component.

Without an entry, the platform shows the vendor's overall status.

### Probes: `[probes]`

`[probes]` changes the probes described in [Probes](#probes).

- The key is the platform name (not case-sensitive).
- The value is `false` to turn the probe off, or a table with `url`, `expect`, or both.
- `url` must be http or https. Probes don't follow redirects.
- `expect` is a code such as `401`, or a class from `"1xx"` to `"5xx"` (any case).
- A `url` without `expect` expects any 2xx. `expect` alone keeps the built-in URL.
- A `[[statuspage]]` vendor has no probe until you give it a `url`.
- A `[[check]]` can't have a probe, because it already is a probe of your endpoint.

The URL shows on screen and in `--json`, so don't put a secret in it.

### Config errors

An invalid config stops aretheydown with exit code 2 and an error that names the file and the problem. A config is invalid if it has any of these:

- **Anywhere:** bad TOML, an unknown key, a duplicate name, or a URL that is not http or https.
- **`hide`:** an unknown name.
- **`[[check]]`:** an `expect` outside 100 to 599.
- **`[aws]`:** an empty or malformed `regions` list.
- **`[components]`:** an entry that is empty, is listed twice, or names an unknown, hidden, or non-Statuspage platform.
- **`[probes]`:** an entry that names an unknown or hidden platform or a check, has a URL that is not http or https, has an `expect` that isn't 100 to 599 or `1xx` to `5xx`, or has no `url` for a platform without a built-in probe.
- **`[alerts]`:** no `webhook`; an empty `webhook` list; a webhook that is not an http or https URL with a host; or an empty `levels` list or an unknown level in it.

A `--config` file that does not exist is also an error.

## Levels

Each platform has one of four levels: `operational`, `degraded`, `outage`, or `unknown`.

- A platform whose fetch fails or takes longer than 10 seconds shows as `unknown`, with the reason in `error`. A `[[check]]` shows `outage` instead (see [Your own endpoints](#your-own-endpoints-check)).
- Scheduled maintenance that is in progress counts as `degraded`, and it is listed as `[maintenance]`. Upcoming maintenance is not shown.

## Exit codes

Scripts can use the exit code to check upstream health:

| Code | Meaning |
| ---- | ------- |
| 0    | Every platform is Operational |
| 1    | At least one platform is Degraded, Outage, or Unknown |
| 2    | Could not run: a usage or setup error |

The interactive screen exits with 0 when you quit, and with 2 if it can't use the terminal.

## Develop

### Set up a new machine

1. Install Rust (1.88 or newer) and `just`:

   ```bash
   brew install rust just
   ```

2. Install prek and turn on the git hooks. prek downloads betterleaks itself.

   ```bash
   brew install prek
   prek install -f
   ```

3. Optional: copy `.env.example` to `.env` and fill in the values. The public status pages need no credentials.

### Commands

| Command        | What it does |
| -------------- | ------------ |
| `just build`   | Debug build |
| `just release` | Optimized release build |
| `just run`     | Run the app (options pass through, such as `just run --once --json`) |
| `just test`    | Run all tests |
| `just lint`    | `cargo fmt --check` and clippy, with warnings as errors |
| `just fmt`     | Format the code |
| `just check`   | Lint and test. Run it before you commit. |

CI (`.github/workflows/ci.yml`) runs on every pull request and every push to `main`. It runs `just check`, a Rust 1.88 build, and the prek hooks.

### Release a new version

1. Bump `version` in `Cargo.toml` and commit. Then tag and push: `git tag v0.3.0 && git push origin v0.3.0`.
2. In `homebrew-tap`, set `tag:` and `revision:` in `Formula/aretheydown.rb`, then commit and push. `revision:` is the tag's commit (`git rev-parse v0.3.0`).
3. On each Mac, run `brew update && brew upgrade aretheydown`.

## License

MIT. See [`LICENSE`](LICENSE).
