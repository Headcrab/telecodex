# Native Codex settings through Telegram

Telecodex uses the configured existing Codex executable, home, account and native
tools. A setting stored for a Telegram topic can override the native CLI default.
Changing a bridge default affects new topics; it does not erase saved choices.

| Setting | Bridge behavior | Change for an existing topic |
| --- | --- | --- |
| Web search | Defaults to `live`; explicitly sends the saved mode to native thread/turn requests. | `/search live`, `/search cached`, `/search off` |
| Model | An omitted `default_model` inherits native settings unless the topic has a saved model. | `/model` |
| Reasoning effort | An omitted `default_reasoning_effort` inherits native settings unless the topic has a saved effort. | `/think default` to inherit; `/think high` for an explicit choice |
| Fast service tier | A topic can save an explicit tier; no explicit tier is sent otherwise. | `/fast on` or `/fast off` |
| Approval and sandbox | Explicit bridge/topic settings are sent to native Codex. The tribal template uses the human-selected `never` and `danger-full-access` baseline. | `/approval` and `/sandbox` |
| Skills, apps, MCP and native tools | Discovered by the existing native harness. The bridge does not send a reduced feature list. Platform/account availability still matters. | Manage in the existing Codex setup. |
| Native memory and hooks | Remain controlled by native configuration and actual handlers. The bridge does not replace or enable them. | Evaluate the installed memory integration separately. |

The tribal template enables live search and full access without interactive
approval. Preserve deliberate receiver overrides and separate credentials,
identity, memory and topic histories. Installing a shared template starts no bot.

CLI slash commands are interface operations, not ordinary model prompts. The
bridge implements a subset and forwards some command-looking text to the model;
that forwarding does not establish native CLI-command parity. Native Plan mode
and pending questions are delivered through [#10](https://github.com/AlterMundi/telecodex/issues/10).
UI affordances, comment streaming and topic history selection differ from the
TUI; those differences do not imply shell, search or reasoning tools are absent.

Native reference: [Codex configuration](https://learn.chatgpt.com/docs/config-file/config-reference).
Owning audit: [#14](https://github.com/AlterMundi/telecodex/issues/14).

## Resuming long conversations

Bound conversations use native `thread/resume` with `excludeTurns: true` by
default. The bridge consumes the returned thread ID and model, then receives
live turn events and typed questions; it does not need a copy of all persisted
turns. This response option preserves the native thread and its model context.
It does not delete history, compact the conversation or replay failed input.
A future history view or delivery-recovery lookup must request the needed turns
explicitly, preferably through native pagination.

The option was verified against the installed Codex 0.160.0 native schema and
proxy: a long existing thread returned 2,149 bytes with zero returned turns,
while its persisted rollout remained unchanged. HTTP/WebSocket/SQLite journeys
verify native questions followed by metadata-only resume and final delivery on
the same thread. Those checks are separate from live Telegram receiving
acceptance after deployment. The bounded 64 MiB transport remains available for
other large native responses.
