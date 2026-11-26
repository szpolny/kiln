# Kiln

A desktop control plane for coding agents, written in Rust with [GPUI](https://www.gpui.rs).

Kiln runs agents as local processes and gives you one place to start, watch, approve, and review their work.

> [!WARNING]
> Kiln is a work in progress. Expect missing features, rough edges, and breaking changes. There are no releases yet, so you have to build it from source.

## Privacy

- No analytics, crash reports, or remote config.
- Projects and threads are stored on your machine.
- Kiln disables Claude Code telemetry and non-essential traffic, and Codex analytics and feedback. Agents still send prompts and the context they read to their model providers.

## Running it

Install and sign in to the CLI for the agent you want to use and make sure it is on your `PATH`. Kiln uses `claude` for Claude Code and the local `codex app-server` for Codex.

With [Nix](https://nixos.org), run `nix develop` (or `direnv allow`) to get the toolchain and the Linux system libraries. Without Nix, install a recent Rust toolchain and, on Linux, the libraries GPUI depends on. On macOS you also need Xcode and its Metal Toolchain:

```sh
xcodebuild -downloadComponent MetalToolchain
```

Then build and launch:

```sh
cargo run -p kiln-app
```

### Local instructions in worktrees

Kiln checks the project checkout for the selected agent's Git-ignored local
instruction file when a worktree has no override of its own. It reads the file
on each turn and passes the text to the agent as context. This works with
existing worktrees and requires no files to be copied into them.

## Development

| Crate | Contents |
| --- | --- |
| `crates/kiln-core` | Domain model, permission policy, and saved state |
| `crates/kiln-agent` | Adapters that run agents as child processes |
| `crates/kiln-app` | The GPUI application (binary `kiln`) |

Run `cargo clippy --all-targets` and `cargo test` before committing. Set `KILN_STATE_DIR` to a scratch folder to keep development runs away from your real data.

## License

[Apache-2.0](LICENSE)

## Acknowledgments

Kiln is heavily inspired by [T3 Code](https://github.com/pingdotgg/t3code).
