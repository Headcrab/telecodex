# Contributing to the maintained fork

Use English for code, documentation, issues, commits and PRs. Link an implementation PR to its owning repository issue and, when applicable, the Matrix parent. Preserve upstream's MIT license and propose focused, generally useful changes upstream after qualification.

Before integration, require current-head CI success and independent review for token handling, ingress durability, session binding, migrations and other security/persistent-state changes. Review fixes against affected invariants; reuse valid evidence instead of restarting unrelated audits.

Run cargo fmt --all --check, locked text-only tests and clippy; optional audio changes must also pass locked all-features tests/clippy. Relevant CI qualifies Linux/macOS text builds with the committed toolchain. Test actual SQLite transactions/restarts, loopback HTTP and native App Server attachment where relevant. Unit tests alone do not prove live Telegram or native model delivery.

Keep credentials, numeric human allowlists, body bindings, private endpoints and conversation databases local. No changes may silently reuse another bot's polling consumer, import unrelated native history, replace owner identity/authentication or introduce Matrix polling/autonomous peer replies. A human-authorized listener starts only through an explicit local setup action.

Document exact deployment reference/hash, feature set, local prerequisites, health checks and rollback. Preserve input journals/offsets and report ambiguous effects honestly. Operational progress belongs in issues or project context, not durable personal memory.
