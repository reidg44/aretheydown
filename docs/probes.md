# Probes

A probe shows what aretheydown saw itself, next to what the vendor says. On every refresh, it sends one GET to the vendor's own service (its front door, not its status page). It sends no credentials and doesn't follow redirects.

If the vendor says Operational but the probe gets no answer, the vendor may have trouble it hasn't posted yet, or your own network may have trouble.

A probe never changes a platform's level, the verdict, the sort order, the exit code, history, or alerts.

[Back to the README](../README.md)

## Default probes

Most front doors answer a request without credentials with a redirect or an error. So each probe has an expected status: an exact code (`401`) or a class (`4xx`). All of these answered as listed on 2026-11-16.

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

To change, turn off, or add probes, use `[probes]` in the config file (see [Config](config.md#probes-probes)).

## Results

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
