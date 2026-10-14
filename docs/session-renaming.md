# Session and topic renaming

Use `/rename <new name>` in a work topic to rename its bound native Codex
session, the bridge label and its Telegram topic. Names accept 1–128 characters
on one line. The command starts no model turn and can run while work is active.
An ordinary private chat without topics updates the session only.

The native operation is App Server `thread/name/set`, with `threadId` and
`name`. Codex owns persistence and native client notifications; the bridge does
not edit Codex's database or session index. The protocol is qualified against
Codex 0.160.0 and its
[native loaded/stored-thread rename tests](https://github.com/openai/codex/blob/rust-v0.160.0/codex-rs/app-server/tests/suite/v2/thread_name_websocket.rs).

Authorized human `forum_topic_edited` name updates also rename the bound Codex
session. Bot echoes and icon-only changes are ignored. A topic without a native
binding saves the selected title and applies it when its first native thread is
bound. Environment synchronization preserves explicit human titles.

There is no transaction spanning Codex and Telegram. The bridge stores human
intent first and reports each result separately. A timeout or API failure is
reported as unconfirmed; retry `/rename` with the intended title to reconcile
the surfaces. Repeated requests set the same name rather than generating new
sessions or replaying prompts. Deleting a bridge session removes its title
override. This implementation does not mirror later CLI name changes back to
Telegram or poll for them.

The local deployment must preserve input journals and existing native bindings.
Install the qualified binary at an idle boundary; retain the prior executable
for rollback and never restore an older conversation database over new input.
