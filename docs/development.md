# Development

[Back to the README](../README.md)

## Set up a new machine

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

## Commands

| Command        | What it does |
| -------------- | ------------ |
| `just build`   | Debug build |
| `just release` | Optimized release build |
| `just run`     | Run the app. Options pass through, such as `just run --once --json`. |
| `just test`    | Run all tests |
| `just lint`    | `cargo fmt --check` and clippy, with warnings as errors |
| `just fmt`     | Format the code |
| `just check`   | Lint and test. Run it before you commit. |

CI (`.github/workflows/ci.yml`) runs on every pull request and every push to `main`. It runs `just check`, a Rust 1.88 build, and the prek hooks.

## Release a new version

Users install from the Homebrew tap [`reidg44/homebrew-tap`](https://github.com/reidg44/homebrew-tap). Its formula builds from this repo at a tag.

1. Bump `version` in `Cargo.toml` and commit. Then tag and push: `git tag v0.3.0 && git push origin v0.3.0`.
2. In `homebrew-tap`, set `tag:` and `revision:` in `Formula/aretheydown.rb`, then commit and push. `revision:` is the tag's commit (`git rev-parse v0.3.0`).
3. On each Mac, run `brew update && brew upgrade aretheydown`.
