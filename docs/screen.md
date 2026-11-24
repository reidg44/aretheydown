# The interactive screen

Run `aretheydown` with no options to open the interactive screen. It refreshes every 60 seconds. Times are in your local time zone.

[Back to the README](../README.md)

## What it shows

The header gives the answer in one line, such as `All 7 operational` or `1 of 7 needs attention: Cloudflare`. A platform that could not be fetched counts as needing attention.

The left pane lists every platform, with trouble first. When data first loads, the top platform is selected. Each row shows:

- the platform's level
- its probe mark: `✓` if its front door answered as expected, `✗` if it didn't, and blank if it has no probe (see [Probes](probes.md))
- the time it was last updated

The right pane shows the selected platform, in this order:

1. A `Probe:` line with the probe's result and URL.
2. Its active incidents. Each one has a title, the affected components, the start time and how long ago that was (`(2h 14m ago)`), the vendor's latest update in full with its stage and age (`Latest update (monitoring, 3h ago):`), and a link. If the platform could not be fetched, you see the error instead.
3. `Resolved in the last 24h (N)`: incidents the vendor resolved in the last 24 hours, each with when it was resolved, how long it lasted, and its link. Only Google Cloud and Statuspage vendors have this list. Resolved incidents never change a platform's level.
4. `History (last 7 days)`: the platform's level changes, newest first. See below.

## Keys

| Key                          | What it does |
| ---------------------------- | ------------ |
| Up/Down or `j`/`k`           | Select a platform (in the timeline: scroll) |
| PageUp/PageDown or `K`/`J`   | Scroll a long incident list. The pane border shows `↑ more` or `↓ more` when there is more to see. |
| Tab and Shift-Tab            | Move a highlight through the selected platform's incidents. It starts on the first and wraps at the ends. |
| `o`                          | Open the highlighted incident in your default browser. If the incident has no link, or there are no incidents, it opens the platform's status page. If the browser can't be opened, the footer says why for a few seconds. |
| `f`                          | Filter the selected platform: on AWS, opens the region picker (see below). On any other platform, the footer says it can't be filtered. |
| `s`                          | Switch between worst-first order (the default: Outage, Unknown, Degraded, then Operational) and config order |
| `t`                          | Open or close the timeline |
| `r`                          | Refresh now |
| `q` or Ctrl-C                | Quit |
| Esc                          | Quit. In the timeline, Esc goes back to the list instead. |

## AWS region picker

Select AWS and press `f` to choose which regions count, instead of editing `[aws] regions` in the config by hand. The picker replaces the list and detail pane with a checklist:

```text
┌ AWS regions ───────────────────────────────────── 2 of 37 ticked ┐
│> [x] us-east-1       US East (N. Virginia)                       │
│  [ ] us-east-2       US East (Ohio)                              │
│  [ ] us-west-1       US West (N. California)                     │
│  [x] us-west-2       US West (Oregon)                            │
```

It lists every AWS region, plus any region code in your config or in the live AWS feed that the built-in list doesn't have. The regions in your `[aws] regions` start ticked.

| Key                     | What it does |
| ----------------------- | ------------ |
| Up/Down or `j`/`k`      | Move |
| Space                   | Tick or untick the region |
| `/`                     | Type a filter: it matches the code or the name, in any case (`ire` finds `eu-west-1  Europe (Ireland)`). Enter stops typing and keeps the filter; Esc clears it. |
| Enter                   | Save and close |
| Esc                     | Clear the filter, or, with none, close without saving |
| `q` or Ctrl-C           | Quit |

Enter writes the ticked regions to `[aws] regions` in the config file the screen loaded (`--config <path>`, else the default path; see [Config file](config.md)). It reads the file again first, so edits made since the screen started are kept, and it changes only the `regions` line: comments and other sections stay as they were. With no config file, it creates one holding only `[aws] regions`. With nothing ticked, it removes `regions` (and an `[aws]` header left empty), so every region counts again. The footer says where the file was written, and the screen fetches again at once with the new regions.

If the save fails (the file can't be written, or it no longer parses), the file stays as it was, the old regions stay in force, and the footer says why for a few seconds.

## History section

The history comes from the file that `aretheydown --serve` records (see [History](history.md)).

```text
History (last 7 days)
  Sat 02:53  Outage → Operational, ongoing
  Sat 02:14  Operational → Outage, lasted 39m
             Elevated error rates on Actions; Degraded Codespaces
  Fri 09:00  Operational, lasted 17h 14m
```

Each entry is a change of level, with the incidents that were active then. A record that only changed incident text doesn't count, and neither does a `--serve` restart. The oldest entry is the level in force when the 7 days start, so an outage that began earlier still shows. An entry more than 6 days old also shows its date (`Sat Nov 8 02:14`).

The screen only reads the history file. It reads it again on each refresh. To read a different file, pass `--history <path>`. If there is no file, the section says `No history: run aretheydown --serve (or brew services start aretheydown) to record it.` If the file can't be read, the message also says why.

## Timeline

Press `t` to swap the list and detail pane for one full-screen timeline. It shows every platform's level changes over the last 7 days, newest first, so you can line up outages on different platforms. The border shows how many changes it lists.

```text
Mon 16 Nov
  07:15  Cloudflare  Degraded → Operational, ongoing
  06:25  Cloudflare  Operational → Degraded, lasted 50m
                     Workers API errors

Sun 15 Nov
  09:04  GitHub      Outage → Operational, ongoing
```

The timeline reads the same history file as the history section. It shows the same entries, rereads the file on each refresh, and shows the same message when there is no history. Press `t` or Esc to go back to the list. The same platform is still selected.
