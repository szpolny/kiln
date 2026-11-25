# Kiln

A desktop control plane for coding agents, written in Rust with [GPUI](https://www.gpui.rs).

Kiln runs agents as local processes and gives you one place to start, watch, approve, and review their work.

> [!WARNING]
> Kiln is a work in progress. Expect missing features, rough edges, and breaking changes. There are no releases yet, so you have to build it from source.

## What works today

- Projects added from local folders, each with its own threads.
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) as the agent. Support for others, such as Codex, is planned.
- A model and a web access setting for each thread.
- Markdown replies, with the tool calls and commands the agent runs.
- Approving or denying actions the agent was refused.

Kiln does not sandbox the agent: your own Claude Code settings still apply, and anything they allow runs without Kiln asking.

## Privacy

- No analytics, crash reports, or remote config.
- Projects and threads are stored on your machine.
- Claude Code is started with its telemetry and non-essential traffic turned off. It still sends your prompts and the context it reads to its model provider.

## Running it

You need the `claude` CLI installed, signed in, and on your `PATH`.

With [Nix](https://nixos.org), run `nix develop` (or `direnv allow`) to get the toolchain and the Linux system libraries. Without Nix, install a recent Rust toolchain and, on Linux, the libraries GPUI depends on. On macOS you also need Xcode and its Metal Toolchain:

```sh
xcodebuild -downloadComponent MetalToolchain
```

Then build and launch:

```sh
cargo run -p kiln-app
```

## Development

| Crate | Contents |
| --- | --- |
| `crates/kiln-core` | Domain model, permission policy, and saved state |
| `crates/kiln-agent` | Adapters that run agents as child processes |
| `crates/kiln-app` | The GPUI application (binary `kiln`) |

Run `cargo clippy --all-targets` and `cargo test` before committing. Set `KILN_STATE_DIR` to a scratch folder to keep development runs away from your real data.

## License

[Apache-2.0](LICENSE)
