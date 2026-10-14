# Native Codex deployment for tribal embodiments

This maintained AlterMundi fork builds on [Headcrab/telecodex](https://github.com/Headcrab/telecodex), retaining its MIT license and native Codex App Server integration. Matrix parent [234](https://github.com/AlterMundi/daimon-matrix/issues/234) and this repository's issue 1 own this delivery. Portable distribution lives in AlterMundi/Skills; secrets, human allowlists, service configuration and body bindings remain local.

## Build the selected revision

Use an exact reviewed 40-character Git commit, the committed Cargo.lock and Rust 1.95.0 (the qualified toolchain in rust-toolchain.toml). Do not resolve a moving branch during deployment. Unix Linux/macOS are the intended text deployment surfaces; CI qualifies both. Existing upstream MSRV 1.85 is retained in Cargo metadata, but this fork's locked release is qualified with 1.95.0, not a claim that every optional platform dependency runs on 1.85.

```bash
cargo build --locked --release --no-default-features
cargo test --locked --no-default-features
```

Text-first deployment needs no transcription model, ONNX runtime or ffmpeg. Audio transcription is an explicit build/configuration choice; upstream's default feature remains available. Build numbers are deterministic (0 unless TELECODEX_BUILD_NUMBER is explicitly set); building never modifies a repository counter.

## Local bot and harness binding

Create a dedicated bot through BotFather and enable its private-chat topics. Do not reuse a bot whose getUpdates stream is already consumed by another gateway. A bot's token file must be a small owner-owned regular file with mode 0600; its containing directory should be 0700. Token file reads reject symlinks, FIFOs and files accessible by another user. Never put the token in repository configuration, command arguments or logs.

Copy tribal.toml.example to a private local configuration and select the exact numeric human Telegram ID before starting. All absolute paths, the allowlist, Codex executable and body-specific settings are local. Preserve the embodiment's existing CODEX_HOME, authentication, owner-selected AGENTS, skills and HMK binding. Do not copy another harness's prompt, identity or memory database.

Set codex.shared_app_server=true to use the existing native `codex app-server proxy`. This requires a Codex version whose native CLI supports that command; verify `codex app-server proxy --help` locally. The managed proxy carries raw WebSocket bytes rather than JSON-lines RPC. This fork performs the native HTTP upgrade and WebSocket framing, as verified against the official release protocol in [Matrix 239](https://github.com/AlterMundi/daimon-matrix/issues/239). The bridge must use the same home/account as the existing interactive harness. Proxy failure is reported; it does not silently select another harness or discard a missing saved thread.

First qualify the existing native proxy without loading bot configuration or starting inference:

```text
telecodex --probe-native /absolute/path/to/existing/codex /absolute/existing/workspace
```

The optional `--create-thread-probes` flag explicitly creates two distinct empty native contexts, still without any model turn. Empty contexts have no persisted rollout before a first native turn, so this check does not qualify restart/CLI resume or live Telegram. Those checks use completed human turns during bot activation. Startup handshake/initialize failures are bounded and do not fall back to a replacement harness.

Explicitly start the configured binary for a human chat listener:

```text
telecodex /absolute/path/to/local/telegram.toml
```

Installing a skill does not start this process, install a user service or read the bot token. A service is a separate explicit owner-local deployment choice. Telegram long polling is authorized ingress for human messages. It creates no Matrix inbox watcher, peer reply service, scheduled inference or identity replacement. Optional history sync/cleanup and lifecycle notifications are disabled by the tribal configuration.

## Topic continuity

The key is the Telegram chat ID plus topic ID. `/topic Name` creates a new topic in a BotFather-enabled private chat or a configured forum. It copies workspace/settings and starts with no inherited Codex thread; unrelated CLI history is never auto-attached in the tribal configuration. The first human turn creates that topic's native thread; subsequent turns resume the saved thread. `/new` starts a fresh native context in the current topic. `/use THREAD_ID_PREFIX` is an explicit human choice to select existing history.

Use `/status` to obtain the native thread identifier when intentionally continuing that conversation with `codex resume THREAD_ID` in the existing CLI. Resuming a Telegram thread does not adopt unrelated private sessions into HMK. Two topics retain separate bindings across a bridge restart even when their workspace is identical.

## Input durability and uncertain effects

Each authorized Telegram update's bounded raw payload and the polling cursor commit in one SQLite transaction before dispatch. Exact duplicate update IDs do not re-execute; the same ID with different content is refused. Queuing/starting a native turn is linked to that admitted update. Text appended to an active turn is durably linked before transmission; acknowledged input shares the turn's outcome. Sent but unacknowledged input stays undetermined and is never automatically submitted again. Only a definite rejection permits queue fallback.

Native thread bindings persist as soon as the harness returns the thread identifier, before the first turn starts. A failed turn retains that binding. An unavailable saved thread also retains its identifier and input; recover its native history or use an explicit human `/new` to create a fresh context.

After a bridge restart, previously unfinished updates retain their raw payload and become undetermined rather than being automatically replayed. `/status` exposes the authorized user's uncertain update IDs. Inspect the topic's native history and explicitly retry only when the human intends it. A delivery failure can leave an externally completed effect uncertain; neither a local offset nor handler completion proves the human received a reply.

The SQLite database contains private human messages and native thread bindings. Keep it owner-local and back it up through SQLite's backup API before upgrades. Keep token files and database snapshots out of GitHub, shared skills and collective memory.

## Release and rollback

Record the exact reviewed source commit, binary SHA256, toolchain, build feature set and current local configuration. Stop the existing consumer before replacing its binary; back up the installed binary/configuration and verify a SQLite snapshot. Preserve the token, allowlist, native auth and thread map. Start one consumer and check getMe, private-topic support, the configured human ACL and two independent topic sessions against the actual native harness.

Rollback replaces the binary and any explicitly changed configuration with their saved versions. Do not automatically restore an older database or roll back the polling offset: that can erase accepted input or repeat external effects. Additive incoming_updates state is retained. If a data recovery is required, first preserve the current database and inspect undetermined updates and native history.

Live bot acceptance requires its dedicated local token and a human-originated message. Unit/loopback/native proxy checks qualify code and harness attachment separately; they do not claim a real Telegram exchange.

## Keep completed messages in Telegram history

A native turn may contain several completed commentary messages before its final
answer. Each completed commentary is published permanently and its Telegram
message references are then protected from later preview edits. The final answer
is published once; finishing a turn whose last message was already committed
does not duplicate that message. Long completed messages are split without
truncating their stored text.

`use_message_drafts=true` uses Telegram drafts only while the current message is
streaming. Those previews are temporary and can disappear. Select
`use_message_drafts=false` for a persistent, edited preview instead. In either
mode, completed commentary must remain in history while the turn continues.
The native thread and input journal remain unchanged.

For completed messages only, set `telegram.show_unfinished_messages=false`.
This suppresses streaming text, tool-progress previews and initial placeholders,
including drafts when `use_message_drafts=true`. Each completed commentary and
the final response are still published permanently, with normal long-message
splitting. Interactive approval prompts and final error reports remain available.
The default is `true`, preserving existing preview behavior.
