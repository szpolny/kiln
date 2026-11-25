# Kiln

A desktop control plane for coding agents, written in Rust with [GPUI](https://www.gpui.rs).

Kiln runs agents as local processes and gives you one place to start, watch, approve, and review their work.

> [!WARNING]
> Kiln is a work in progress. Expect missing features, rough edges, and breaking changes. There are no releases yet, so you have to build it from source.

## What works today

- Projects added from local folders, each with its own threads. A project shows the icon found in its folder, such as `favicon.svg`, `assets/logo.png`, or the one its `index.html` links to, and its initials otherwise. To use another image, click the project name above a thread and choose **Change icon…**.
- One thread list across projects, with each thread's branch. Settle the threads you are done with to move them out of the way; sending a message brings one back.
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) and [Codex](https://developers.openai.com/codex/). Choose either agent in **New thread**, or pick one of its models from the composer. Choosing a different provider after a conversation has started opens a new thread in the same project.
- A model, a reasoning level, and a permission mode for each thread. **Manual** shows the provider's approval requests and lets you control web search. Claude Code can ask before a search or fetch. Codex's built-in search requires **Allow**. **Auto** uses the provider's automatic review where available. **Full access** skips routine approval prompts. Switch modes between turns using the thread toolbar.
- Set the permission mode that new threads start with in **Settings**.
- Markdown replies, with the tool calls and commands the agent runs. Once the agent has replied, the work before the reply folds under **Worked for …**, which also shows when the reply arrived; click it to see the work again.
- Approving or denying actions from the same cards in either provider. For Codex the current turn waits for your decision.
- Stopping a running turn with the stop button or Escape.

The providers enforce their own tool permissions. Claude Code applies its configured rules. Codex uses a workspace-write sandbox in Manual and Auto modes. Commands Codex classifies as trusted can run without a prompt. An approved escalation can run outside that sandbox.

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

## Development

| Crate | Contents |
| --- | --- |
| `crates/kiln-core` | Domain model, permission policy, and saved state |
| `crates/kiln-agent` | Adapters that run agents as child processes |
| `crates/kiln-app` | The GPUI application (binary `kiln`) |

Run `cargo clippy --all-targets` and `cargo test` before committing. Set `KILN_STATE_DIR` to a scratch folder to keep development runs away from your real data.

## License

[Apache-2.0](LICENSE)
