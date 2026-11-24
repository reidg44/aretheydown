# Web page and JSON API

`aretheydown --serve` keeps running and serves the latest result over HTTP. A browser tab, scripts, other machines, or a status bar can read it without waiting for a new fetch.

[Back to the README](../README.md)

```bash
aretheydown --serve
open http://localhost:7341/
curl -s localhost:7341/api/status | jq
```

It fetches every platform on start and then every 60 seconds. Requests only read the latest result. While it runs, it also records [history](history.md) and sends [alerts](alerts.md).

## Addresses

| Address           | What it returns |
| ----------------- | --------------- |
| `GET /`           | A web page with the same picture as `--once` |
| `GET /api/status` | `200` with the same JSON as `--once --json` (see [Output](output.md#json)) |
| `GET /healthz`    | `200 ok` |

The web page shows the verdict and the platforms, worst first. For each incident it shows the stage, age, components, full latest update, and link. It also shows the incidents resolved in the last 24 hours and each platform's status page. The page refreshes itself every 60 seconds without reloading. With JavaScript off, it still shows but doesn't refresh. It follows your OS's light or dark mode.

Until the first fetch ends:

- `GET /` returns `503` with a short page that reloads itself every few seconds.
- `GET /api/status` returns `503` with `{"error": "first fetch still running"}`.

The HTTP status never reports platform health. A platform in Outage still returns `200`, so read `level`.

## Options

| Option             | What it does |
| ------------------ | ------------ |
| `--port <port>`    | Listen on this port (default 7341) |
| `--bind <addr>`    | Listen on this address (default `127.0.0.1`). `0.0.0.0` opens it to the network. |
| `--history <path>` | Write history to this file (see [History](history.md)) |
| `--config <path>`  | Use this config file (see [Config](config.md)) |

The server has no authentication, so it warns you when the address can be reached from the network.

To stop the server, press Ctrl-C or send SIGTERM. It exits with code 0.

## Run in the background

If you installed with Homebrew, `brew services` keeps `aretheydown --serve` running as a launchd user agent. It starts when you log in and restarts if it exits. It serves the page at `http://127.0.0.1:7341/`, records history, and sends alerts.

```bash
brew services start aretheydown
curl -s http://127.0.0.1:7341/healthz
brew services list
```

- To stop it, run `brew services stop aretheydown`. It gets SIGTERM and exits with code 0.
- After you change the config file, run `brew services restart aretheydown`. It reads the config only when it starts.
- Its startup lines and warnings go to `$(brew --prefix)/var/log/aretheydown.log`.
- It runs with no options, so it uses the default paths: the config at `~/.config/aretheydown/config.toml` and the history at `~/.local/state/aretheydown/history.jsonl`. `aretheydown --at …` and the interactive screen's history section read that same history.
