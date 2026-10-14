# Native event routing and turn completion

Telecodex uses the same native Codex App Server motor as the CLI. The bridge is
responsible for receiving and presenting the correct thread's events while that
motor owns model sampling, tool execution, child-agent waits and completion
notification injection. Enabling the hook framework does not install handlers or
create a separate callback scheduler.

## Reproduced bridge failures

Two actual WebSocket/HTTP/SQLite scenarios against the previous bridge artifact
established these failures with synthetic data:

- A foreign thread's completed final answer was published in the parent topic and
  closed the parent's foreground receiver before its own final answer arrived.
- The parent's own completed final answer closed the receiver while the native
  turn could still produce a later continuation. That continuation required no
  additional human input in the fixture, but Telegram never received it.

These findings are not a claim that a specific historical message had either
trigger. Local native/bridge timestamps showed premature completion, but do not
identify every historical cause. No private message contents or route identifiers
are needed in public evidence.

## Resulting behavior

Shared transports require the selected native thread scope and correlate turn
and item events with the accepted active turn. Foreign thread starts, answers,
completions, idle transitions and approval requests cannot change a topic's
binding, output or completion. Stale completions from another turn in the same
thread are ignored. Legacy private stdio events may omit scopes; explicit scope
mismatches still fail routing. A transport ending before native completion is
reported as a failed/uncertain delivery, not a successful completed turn.

Completed final answers are published permanently as soon as they arrive, like
completed commentary. They do not close the native connection. The receiver
continues until its matching native turn completes, or its own thread becomes
idle after a completed answer. Subsequent work remains steerable, and subsequent
messages do not edit a previously committed answer. Delivery/ingress outcomes
remain durable; no prompt is replayed and no extra model turn is started.

A detached shell process finishing is not by itself a native assistant wakeup.
The agent must keep waiting/polling it through the harness or use an explicitly
provided completion mechanism. This correction does not invent automatic
background-process callbacks or turn child results into new prompts after a
completed native turn.

## Source comparison

The inspection used native Codex tag `rust-v0.160.0`:

- [CLI event targets](https://github.com/openai/codex/blob/rust-v0.160.0/codex-rs/tui/src/app/app_server_event_targets.rs)
  distinguish thread-scoped and global notifications before routing them.
- [Native agent control](https://github.com/openai/codex/blob/rust-v0.160.0/codex-rs/core/src/agent/control.rs)
  watches child status and injects completion into the parent; that path belongs
  to the shared motor, not a Telegram hook. The v2 completion path uses
  `trigger_turn=false`; the legacy path injects a fragment without a new turn.
- [Turn notification contracts](https://github.com/openai/codex/blob/rust-v0.160.0/codex-rs/app-server-protocol/src/protocol/v2/turn.rs)
  carry the thread and turn identity separately from an individual answer.
- [Official App Server lifecycle](https://learn.chatgpt.com/docs/app-server)
  identifies native turn completion and item completion as different events.

Owning investigation: [Telecodex #8](https://github.com/AlterMundi/telecodex/issues/8).
The related activity indicator is tracked in #11. Runtime deployment preserves
native thread bindings, the ingress journal and polling offsets. A binary rollback
must preserve the database, and a cutover must not cancel active foreground work.
