# Project instructions

Keep changes focused and preserve native Codex thread ownership, durable ingress,
and explicit delivery uncertainty. Read [project context](memory/docs/context.md)
and [contribution rules](CONTRIBUTING.md) before changing these contracts.

Use the committed Rust toolchain and Cargo.lock. Validate with cargo fmt, locked
text tests, clippy, and the affected loopback HTTP/SQLite journeys. Audio changes
also require all-features tests and clippy. Do not use live credentials or start
model turns as part of automated qualification.
