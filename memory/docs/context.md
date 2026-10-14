# Runtime context

The Rust bridge admits Telegram updates into SQLite, dispatches each topic in
order, and attaches to native Codex threads through the App Server protocol.
Codex owns conversation history and inference. The bridge owns Telegram topic
bindings, authorized input admission, pending human controls and output delivery.

## Contracts and domain model

- [Native event routing](../../docs/native-event-routing.md): thread/turn ownership
  and lifecycle events.
- [Pending questions](../../docs/native-questions.md): requester, topic, request,
  question and message correlation, plus restart and uncertain-answer semantics.
- [Native settings](../../docs/native-settings.md) and
  [session renaming](../../docs/session-renaming.md): persistent session controls.
- [Deployment](../../docs/tribal-deployment.md): local prerequisites, qualification
  and rollback. Keep credentials and conversation databases outside version control.
- [Convergence research](../../docs/telegram-convergence.md): provenance and the
  distinction between synthetic qualification and human Telegram acceptance.
- [Optional activity observer](../../docs/activity-indicator.md): read-only native
  metadata and ingress-journal observation, private state, opt-in activation and
  independent Python qualification. The Rust bridge remains the sole update consumer.

## Integration decisions

The topic icon picker and native question controls use separate callback prefixes
and separate ephemeral state. Both retain existing authorization checks.

A failed question receipt or answer-journal write must preserve a live pending
dialogue. Reopening through /questions retries display only; already accepted
answers are retained without creating or replaying a native turn.

Completed output waits for Telegram retry_after and retries publication within a
bounded budget. Only acknowledged permanent messages can be committed. Partial
multi-chunk delivery reuses acknowledged message IDs rather than sending duplicates.

Use the synthetic loopback suites in tests/ to qualify these boundaries without
live Telegram or model calls. Integration failures must be reproduced before a
fix is accepted; format/lint success alone does not establish delivery correctness.
