# Deployment Report

## What Was Deployed

- Local `Telecodex` checkout in [`D:/GitStorageG/RemoteCodex`](D:/GitStorageG/RemoteCodex)
- Local release binary at [`target/release/telecodex.exe`](D:/GitStorageG/RemoteCodex/target/release/telecodex.exe)
- Local config in [`telecodex.toml`](D:/GitStorageG/RemoteCodex/telecodex.toml)
- Local secret storage in [`.env`](D:/GitStorageG/RemoteCodex/.env)
- Local process launchers in [`scripts`](D:/GitStorageG/RemoteCodex/scripts)
- Local runtime state in `telecodex.sqlite3`, `logs`, `run`, and `tmp`

## Current Architecture

- Telegram bot talks to a local long-polling `Telecodex` process on this Windows PC.
- `Telecodex` calls the local `codex` CLI and stores its own chat/session state in SQLite.
- Continuity comes from Telecodex session persistence plus local Codex history import by `cwd`.
- Access is limited by `startup_admin_ids`, currently only Telegram user `492309203`.
- The dedicated forum dashboard is supported, but not configured yet; private chat mode is currently active.

## Important Windows Note

The repository does not build out of the box on this PC with the stock Visual Studio 2022 Community toolchain `14.38`. `onnxruntime` from `ort-sys` links successfully only when Telecodex is built against a newer MSVC STL. The working local build uses:

- Rust toolchain `1.85.0`
- `MSVC 14.44.35207` from [`D:/GitStorageG/RemoteCodex/.msvc-kit`](D:/GitStorageG/RemoteCodex/.msvc-kit)
- Windows SDK `10.0.26100.0` from the local machine

The PowerShell scripts encapsulate this so normal start/stop does not require manual env setup.

## Continuity vs VS Code Extension

What is preserved:

- Telecodex chat session metadata
- selected workspace / cwd
- Codex thread binding when Telecodex can resolve it
- imported local Codex Desktop and CLI history by workspace

What is not preserved:

- live in-memory state of the VS Code extension runtime
- editor UI state, open tabs, selection, temporary extension-side context
- undocumented internal extension session objects

So this is close continuity via `Codex CLI + Telecodex + local Codex history`, not a 1:1 mirror of a live VS Code extension session.
