# Alerts

`aretheydown --serve` can tell you when a platform's level changes, so you hear about an upstream outage before you start troubleshooting your own systems. It POSTs a JSON alert to one or more webhooks, such as a Slack incoming webhook.

[Back to the README](../README.md)

## Set it up

Add a webhook to the [config file](config.md):

```toml
[alerts]
webhook = "https://hooks.slack.com/services/…"
```

`webhook` also takes a list, such as `webhook = ["https://hooks.slack.com/services/…", "http://127.0.0.1:9000/hook"]`. Each alert goes to every URL. A URL listed twice gets each alert once.

**Keep the webhook URL secret.** Anyone who has it can post to it. aretheydown never prints it, not even in config errors.

Only `--serve` sends alerts. The interactive screen and `--once` ignore `[alerts]`.

## When it sends an alert

A platform's level must change, in either direction (for example Operational → Degraded, Outage → Operational, or to or from Unknown). The new level must also hold for two polls in a row, about 2 minutes. Then `--serve` POSTs one JSON alert for that platform.

- A one-poll blip, such as a single failed fetch, sends nothing.
- The first poll after start only notes each platform's level.

## Hear about fewer changes

Add `levels`:

```toml
[alerts]
webhook = "https://hooks.slack.com/services/…"
levels = ["outage"]
```

With `levels`, only a change into or out of a listed level sends an alert. Spell levels the same way as `--json`'s `level`: `operational`, `degraded`, `outage`, `unknown`. For example, with `["outage"]`:

- Degraded → Outage and Outage → Operational send an alert.
- Operational → Degraded and Degraded → Operational do not.

If you leave out `unknown`, a feed that could not be read (Operational → Unknown) stays quiet. Without `levels`, every change sends an alert.

## What an alert looks like

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

## If a send fails

A send fails if there's no answer within 10 seconds, or the answer isn't 2xx. Then `--serve` prints a warning that names the platform and the webhook's place in the list (`webhook 2`). That alert is dropped, not retried. The other webhooks still get theirs, and polling goes on.
