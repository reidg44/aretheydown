# History

`aretheydown --serve` keeps a history of what it saw, so you can look back later. For example: "did that failed deploy line up with an outage?"

You can read the history two ways:

- `aretheydown --at <time>` prints what every platform looked like at that time.
- The interactive screen shows each platform's last 7 days, and `t` opens a timeline of all of them (see [The interactive screen](screen.md#history-section)).

[Back to the README](../README.md)

## Look back with `--at`

```bash
aretheydown --at 02:14
aretheydown --at "yesterday 02:14"
aretheydown --at "2h ago"
aretheydown --at "2026-11-14 02:14 UTC" --json
aretheydown --at 2026-11-14T02:14:00Z
```

For each platform, `--at` shows the last record at or before that time. The output is the same text as `--once`, under a first line such as `At 2026-11-14 02:14 (local), from history recorded by --serve:`. Add `--json` for the same JSON as `--once --json`. `--at` reads no config and fetches nothing.

`--at` accepts these times:

| Form                 | Example |
| -------------------- | ------- |
| RFC 3339             | `2026-11-14T02:14:00Z` |
| Now                  | `now` |
| Relative             | `30m ago`, `1d 4h ago`, `1h30m ago` (number-and-unit groups using `m`, `h`, or `d`, then `ago`) |
| Local clock time     | `HH:MM`, `today HH:MM`, `yesterday HH:MM`, or `YYYY-MM-DD HH:MM` |

A bare `HH:MM` means the most recent one. At 09:00, `--at 23:00` means yesterday at 23:00, but `today 23:00` means today. To read a clock time in UTC, end it with ` UTC` or `Z` (`yesterday 02:14 UTC`, `02:14Z`). `today` and `yesterday` are then UTC days too. Except for RFC 3339, case and extra spaces don't matter.

Each status shows as it was when it was recorded. Ages count from when it was fetched. Its `resolved in the last 24h` line is the one it had then. A later resolution that didn't change the level shows only in a later record.

`--at` only sees what `--serve` recorded or filled in from vendor feeds:

- A platform with no record is not shown.
- A platform taken from a filled-in record says so after its description: `1 active incident (from vendor history)`. With `--json`, it has `"backfilled": true`.
- During a time when `--serve` was not running, a platform shows the last status recorded before it stopped.

Exit codes match `--once`: 0 if all are Operational, 1 otherwise. Exit code 2 means a bad time, a missing history file, or nothing recorded by that time. In the last case, the message gives the earliest record. Lines in the file that can't be read are skipped with a warning.

## The history file

- **What it holds.** Each poll adds a platform's status to the file only when the status changed. A change is a new level, description, error, or set of active incidents (including their stage and latest update). A new fetch time or a new resolved list alone is not a change. So the file holds one line per change, not one per poll. The first poll after each start writes every platform.
- **Format.** JSON Lines: one `{"recorded_at": <RFC 3339>, "status": <platform>}` object per line. The platform has the same fields as `--json` (see [Output](output.md#json)), except `age_seconds` and `probe`.
- **Where it lives.** `$XDG_STATE_HOME/aretheydown/history.jsonl`, or `~/.local/state/aretheydown/history.jsonl` if `XDG_STATE_HOME` isn't set. Use `--history <path>` to pick another file, for `--serve`, `--at`, or the interactive screen. The folder is created if it's missing.
- **How long it keeps records.** Each time `--serve` starts, it drops records older than 30 days.
- **If a write fails,** `--serve` prints a warning and keeps serving.

## Filling in history from vendor feeds

When `--serve` starts, before it records its first poll, it fills in the time before it first ran. It uses the vendors' own incident history. This lets `--at` and the interactive screen look back further than `--serve` has been running.

How it works:

- It reads the vendors' `incidents.json` feeds. Only Statuspage vendors and Google Cloud have them. AWS, Azure, and Microsoft 365 publish no history, so they get none.
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
