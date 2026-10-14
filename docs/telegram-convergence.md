# Telegram harness convergence pilot

The pilot transfers useful interaction patterns into the existing native Codex
frontend. It preserves each harness's backend and existing session, authentication
and memory boundaries. It does not require a new shared agent runtime first.
Parent: [Matrix #262](https://github.com/AlterMundi/daimon-matrix/issues/262).

## Current delivery — 2026-10-08

The active qualified runtime is
`ee5f8837ab51da99034fcd4285b0c7916c96651f`, integrated through downstream
[PR #26](https://github.com/AlterMundi/telecodex/pull/26) at master `3a6dbe8`.
The canonical shared package is `telegram-codex` 1.1.5, with an immutable source
pin and archive digest. Its selective installation and neutral surface check
passed. The safe cutover completed: the installed executable matches the frozen
candidate, native bindings and ingress journals were preserved, and Telegram
accepted the readiness notice. There is no pending cutover. Installation receipts are distinct from human acceptance. Nicolás subsequently
reported continued use of the same HMK session without the previous error or
observed side effects; the long-history repair has that receiving acceptance.

Existing threads now resume with native `excludeTurns: true`, retaining context
and persisted history without sending all turns back to the bridge. The bounded
64 MiB frame/message limit from PR #23 remains for other large responses. Two
real Codex 0.160.0 proxy probes resumed long threads with roughly 2 KiB metadata
responses and unchanged persisted rollouts, without starting model turns.
Qualification passed 159 locked Rust text tests, 16 question HTTP/WebSocket/SQLite
journeys, ingress regressions, fmt/clippy and Linux/macOS text/activity plus
Linux audio CI. The optional observer remains at qualified source `e77feab`,
with 27 file/SQLite/HTTP tests.

Human Telegram acceptance on the earlier `5af79bd` baseline verified genuine
Plan mode, question reopening and free-text Other answer through ForceReply with
same-turn continuation. CLI resume was qualified on the native integration
baseline. Nicolás confirmed continued HMK use after activation without error recurrence or
observed side effects. A fresh Telegram/CLI rename-handoff exchange and adoption
by another embodiment remain separate, unscheduled receiving evidence.

The runtime proposal [Headcrab/telecodex #15](https://github.com/Headcrab/telecodex/pull/15)
includes the long-history corrections at source head `2a12240`, whose CI passed.
It is open and ready for review. The optional observer proposal
[Headcrab/telecodex #16](https://github.com/Headcrab/telecodex/pull/16)
is an open draft with an explicit dependency on the transport proposal.
Neither proposal has been merged or adopted upstream.

## Unscheduled backlog

Nicolás deferred the remaining work: no implementation is currently planned.
Use actual usage to decide which improvement to activate next. Pending items:

- [Expired question quote recovery #24](https://github.com/AlterMundi/telecodex/issues/24):
  improve the notice and investigate the ForceReply lifecycle; preserve exact
  request ownership and avoid replaying closed answers as new prompts.
- [Client-mediated interactions #18](https://github.com/AlterMundi/telecodex/issues/18):
  select and qualify one useful native interaction beyond the delivered question broker.
- [Disappearing-preview investigation #8](https://github.com/AlterMundi/telecodex/issues/8):
  correlate any remaining report with actual delivery evidence before changing code.
- [Mini App research #20](https://github.com/AlterMundi/telecodex/issues/20):
  propose the smallest optional per-topic detail view; implementation is not authorized yet.
- Reconcile remaining receiving/handoff acceptance in Matrix #234 and qualify
  another receiving embodiment when the human selects that work.
- Follow upstream adoption and record measured transfer costs and maintenance delta.
- Evaluate a native handback shortcut and optional tool/todo presentation as later imports.

Cross-topic admission under stalled steering was implemented in `2580fce` and
qualified with ingress I/O across five active conversations. Native modes,
questions, rename, defaults, activity and long-history repairs are delivered;
they must not be listed again as unimplemented prerequisites. The disappearing
preview remains an independent unverified case.

## Bounded source comparison

These are inspected source baselines, not claims of current feature parity or
live receiving acceptance in every harness:

| Source and pinned revision | Useful behavior and evidence | Decision for this delivery |
| --- | --- | --- |
| [Headcrab/telecodex `0aa3231`](https://github.com/Headcrab/telecodex/tree/0aa3231e98748364a4fcb63b6ebcf73199c5a2c0) | README and inherited runtime: independent topics, native thread binding, steering, attachments/artifacts, authentication, settings, history and rate-aware Telegram output. | Reuse the existing Rust bridge. Preserve its license and submit generally useful changes upstream. |
| [AlterMundi/telecodex `289d972`](https://github.com/AlterMundi/telecodex/tree/289d972f64be4cc55d2ec00f7e7b4ff00bfd13c1) | Native proxy/SQLite/HTTP qualification: durable ingress, completed messages, owner-selected search/access defaults, synchronized rename, scoped event reception and post-final continuation. | Keep this qualified behavior while adding native modes and questions. The activity indicator remains a separately qualified optional companion. |
| [NousResearch/hermes-agent `0c2ff85`](https://github.com/NousResearch/hermes-agent/tree/0c2ff854a5c5121c75ac0b0a35241a580ee27fb7) | [`send_clarify` and callback dispatch](https://github.com/NousResearch/hermes-agent/blob/0c2ff854a5c5121c75ac0b0a35241a580ee27fb7/plugins/platforms/telegram/adapter.py), plus [clarification-button tests](https://github.com/NousResearch/hermes-agent/blob/0c2ff854a5c5121c75ac0b0a35241a580ee27fb7/tests/gateway/test_telegram_clarify_buttons.py): complete choices in the body, short controls, an Other text path, authorized callbacks and expired-request feedback. | Adapt the interaction to Codex's existing typed request protocol. Do not copy Hermes's process-global clarification registry or invent another model tool. Keep full option descriptions and stale-control feedback. |
| [benedict2310/telecodex `fd2a241`](https://github.com/benedict2310/telecodex/tree/fd2a24134f0459e15df877bd5c8c7fc7455253fd) | README advertises per-context sessions, tool verbosity, live todo display, handback commands and optional reactions/usage. These advertised features were not independently live-qualified in this pilot. | Retain native resume handoff and topic independence. Prefer the current human-selected minimal output. Verbosity, todo presentation and a dedicated handback shortcut are subsequent candidates, not substitutes for working native questions. |

## Combined behavior and qualification

The first useful increment adds genuine topic-local `/plan`, `/default` and
`/questions` to the already functioning Telegram frontend. Buttons and quoted
or explicitly armed text respond to the exact native request; ordinary messages
still steer. Multiple questions, asynchronous work, stale controls, concurrent
topics, cancellation, unavailable modes and uncertain delivery have actual
HTTP/WebSocket/SQLite journeys in `tests/questions_io.py`. See
[native-questions.md](native-questions.md) for the public contract and limitations.

A disposable native Codex 0.160.0 qualification used the real App Server motor
with loopback Telegram HTTP: Codex generated a question, the bridge rendered its
choices, a synthetic authorized human chose one, and Codex continued with that
choice in the same completed native turn. This establishes native integration,
separate from the subsequent real human Telegram acceptance recorded above.

The portable `telegram-codex` package remains one canonical directory in
[AlterMundi/Skills](https://github.com/AlterMundi/Skills/tree/main/skills/telegram-codex).
Its immutable source reference, archive hash, setup and recovery instructions
carry the qualified feature set to another Codex embodiment. Reading the skill
starts no listener. Installing it does not select a bot, copy another being's
identity/history or enroll anyone in Matrix. Hermes continues using its own
native gateway. Receiving acceptance belongs to each actual deployment.

## Next imports

Keep the next cycle small: choose one useful capability, cite its source, adapt
it to the receiving harness, qualify its changed boundaries, and propose the
focused upstream change. A dedicated native handback shortcut and configurable
tool/todo presentation can use the existing motor rather than a replacement
runtime. Broader watching and community analysis remain parent work; they do
not block this functioning increment. Generic skills and cross-being adoption
remain attributed choices with receiving acceptance.
