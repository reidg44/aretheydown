# Text and JSON output

`aretheydown --once` checks every platform once, prints the result, and exits. Add `--json` for JSON. `aretheydown --at` and `aretheydown --serve` use the same formats.

[Back to the README](../README.md)

## Text

The output starts with the verdict, then lists platforms worst first. Each incident's latest update fits on one line, cut to about 200 characters. A platform that resolved incidents in the last 24 hours gets one more line about them.

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

## JSON

```bash
aretheydown --once --json
```

This prints a JSON array with one object per platform, in config order. Unlike the text, JSON keeps each update's text whole.

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

## Levels

Each platform has one of four levels: `operational`, `degraded`, `outage`, or `unknown`.

- A platform whose fetch fails or takes longer than 10 seconds shows as `unknown`, with the reason in `error`. A `[[check]]` shows `outage` instead (see [Config](config.md#your-own-endpoints-check)).
- Scheduled maintenance that is in progress counts as `degraded`, and it is listed as `[maintenance]`. Upcoming maintenance is not shown.

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0    | Every platform is Operational |
| 1    | At least one platform is Degraded, Outage, or Unknown |
| 2    | Could not run: a usage or setup error |

The interactive screen exits with 0 when you quit, and with 2 if it can't use the terminal.
