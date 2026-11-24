# Kiln

Kiln is a privacy-first control plane for coding agents, written in Rust with GPUI. It runs agents (Claude Code, Codex, and others) as local processes and gives the user one fast native place to start, watch, approve, and review their work. The UI is a project and thread sidebar, a focused thread view, and a calm composer.

## Principles

1. **Private by default.** No analytics, crash reports, feedback, usage data, or remote config. Builds never contact services run by the project. Exporting traces to a receiver the user configures is the only allowed outbound diagnostics path.
2. **Local first.** State lives on the user's machine. Network access, file access outside the project, and tool execution by agents are explicit, visible, and revocable.
3. **Simple systems.** Build the smallest model that makes behavior unsurprising. Fight scope creep.
4. **Native performance.** Keep rendering cheap: virtualize long lists, avoid per-frame allocation, and avoid sending or copying more data than a view needs.

## Layout

- `crates/kiln-app`: the GPUI application (binary `kiln`).

## Commands

- `cargo build` / `cargo run -p kiln-app`: build and launch. macOS needs Xcode's Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`).
- `cargo clippy --all-targets` and `cargo test`: run before committing.

## Process hygiene

- Never kill processes by name or pattern. Kill only a PID you captured when you spawned it.
- Do not read or write the user's real Kiln data directory in tests; use a temp directory.

## Commits and pull requests

- Conventional Commits, plain language: `feat(app): add thread sidebar`.
- PR body: the problem in a sentence or two, then how it was solved.
