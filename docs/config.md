# Config file

The config file is optional. Without one, aretheydown checks only the built-in platforms. With one, you can:

- add vendors that use Atlassian Statuspage (`[[statuspage]]`)
- check your own endpoints (`[[check]]`)
- hide built-in platforms (`hide`)
- count only the AWS regions you use (`[aws] regions`)
- watch only some components of a Statuspage platform (`[components]`)
- change or turn off probes (`[probes]`)
- send `--serve` alerts to webhooks (`[alerts]`, see [Alerts](alerts.md))

[Back to the README](../README.md)

## Where it lives

`~/.config/aretheydown/config.toml`, or `$XDG_CONFIG_HOME/aretheydown/config.toml` if `XDG_CONFIG_HOME` is set. The path is the same on every OS. To use another file, pass `--config <path>`.

[`config.example.toml`](../config.example.toml) has a commented example. Here is a short one:

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

## Your own endpoints: `[[check]]`

A `[[check]]` adds one of your own endpoints as a platform, listed after the vendors. Then the first screen answers "is it me or them?"

Each check is a plain GET, and it follows redirects:

- The expected status is `operational`, with a description such as `Responding: HTTP 200 in 142 ms`. The expected status is `expect` if you set it (100 to 599), or any 2xx if you don't.
- Any other status is `outage`, such as `HTTP 503 (expected 2xx)`.
- No connection (`could not connect`) or no answer within 9 seconds (`timed out after 9s`) is also `outage`.

A check is never `unknown`. Because it follows redirects, a 3xx `expect` only matches a redirect that has no `Location` to follow.

History records a check when its level or status code changes, not each time its response time changes. A check shows everywhere a vendor does (the interactive screen, `--once`, `--json`, the `--serve` page, history, and alerts). It counts in the verdict and exit code, and `o` opens its URL.

The URL shows on screen and in `--json`, so don't put a secret in it.

## AWS regions: `[aws] regions`

`[aws] regions` makes AWS count only events in those region codes. Then a long-running event in a region you don't use doesn't keep AWS at `outage`. Global events (not tied to a region) always count. The AWS line says how many events in other regions it left out. Without `regions`, every region counts.

You can also pick the regions on the interactive screen: select AWS and press `f` (see [AWS region picker](screen.md#aws-region-picker)). The screen writes `[aws] regions` into this file and changes nothing else in it. A running `aretheydown --serve` reads the config only when it starts, so it uses the new regions after its next start (`brew services restart aretheydown`).

## Statuspage components: `[components]`

`[components]` does the same for Statuspage platforms: GitHub, Cloudflare, OpenAI, Anthropic, and any `[[statuspage]]` vendor.

- The key is the platform name.
- The value is a list of component or group names, as the status page shows them. A group name, such as `"North America"`, covers every component in that group.
- Neither key nor names are case-sensitive.

With a list, the platform's level comes from those components' status and the incidents that touch them, not from the vendor's overall status. For example, if you only use Workers, Cloudflare datacenter maintenance doesn't turn Cloudflare yellow. Incidents that list no components always count.

The platform's line reads like `1 of 1 watched components not operational: Workers (1 incident on other components not shown)`. If a name you listed isn't in the feed, the line ends with `(not in feed: "…")`. This can happen when a vendor renames a component.

Without an entry, the platform shows the vendor's overall status.

## Probes: `[probes]`

`[probes]` changes the [probes](probes.md).

- The key is the platform name (not case-sensitive).
- The value is `false` to turn the probe off, or a table with `url`, `expect`, or both.
- `url` must be http or https. Probes don't follow redirects.
- `expect` is a code such as `401`, or a class from `"1xx"` to `"5xx"` (any case).
- A `url` without `expect` expects any 2xx. `expect` alone keeps the built-in URL.
- A `[[statuspage]]` vendor has no probe until you give it a `url`.
- A `[[check]]` can't have a probe, because it already is a probe of your endpoint.

The URL shows on screen and in `--json`, so don't put a secret in it.

## Config errors

An invalid config stops aretheydown with exit code 2 and an error that names the file and the problem. A config is invalid if it has any of these:

- **Anywhere:** bad TOML, an unknown key, a duplicate name, or a URL that is not http or https.
- **`hide`:** an unknown name.
- **`[[check]]`:** an `expect` outside 100 to 599.
- **`[aws]`:** an empty or malformed `regions` list.
- **`[components]`:** an entry that is empty, is listed twice, or names an unknown, hidden, or non-Statuspage platform.
- **`[probes]`:** an entry that names an unknown or hidden platform or a check, has a URL that is not http or https, has an `expect` that isn't 100 to 599 or `1xx` to `5xx`, or has no `url` for a platform without a built-in probe.
- **`[alerts]`:** no `webhook`; an empty `webhook` list; a webhook that is not an http or https URL with a host; or an empty `levels` list or an unknown level in it.

A `--config` file that does not exist is also an error.
