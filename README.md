# <div align="center">🤖 Telecodex</div>

<div align="center">
  <strong>Telegram as a remote, topic-aware frontend for your local Codex CLI.</strong>
</div>

<div align="center">
  Long polling. Persistent sessions. Attachment handling. Topic-aware workspace sync. SQLite ACLs.
</div>

<br />

<div align="center">

[![Rust](https://img.shields.io/badge/Rust-1.85%2B-000000?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
[![Telegram Bot API](https://img.shields.io/badge/Telegram-Bot%20API-26A5E4?style=for-the-badge&logo=telegram)](https://core.telegram.org/bots/api)
[![SQLite](https://img.shields.io/badge/SQLite-Storage-003B57?style=for-the-badge&logo=sqlite)](https://www.sqlite.org/)
[![Codex CLI](https://img.shields.io/badge/Codex-Local%20CLI-111111?style=for-the-badge)](https://github.com/openai/codex)
[![License: MIT](https://img.shields.io/badge/License-MIT-EA580C?style=for-the-badge)](./LICENSE)

</div>

<div align="center">

[🇷🇺 Русская версия](./README.ru.md) • [📄 MIT License](./LICENSE)

</div>

---

## ✨ What it is

**Telecodex** is a Rust bridge that connects a local `codex` CLI instance to Telegram.

It turns Telegram chats and forum topics into lightweight remote workspaces where you can:

- talk to Codex from your phone or desktop Telegram client,
- keep separate sessions per chat/topic,
- switch between existing Codex threads,
- send files and media into a turn,
- receive streamed progress and generated artifacts back in Telegram.

No webhook infrastructure. No browser dependency. No cloud relay between Telegram and your local Codex process.

## 🔥 Why it is useful

- **Remote terminal vibe, but usable**: Telegram becomes the UI, Codex stays local.
- **Topic-aware sessions**: each forum topic can map to its own workspace/session.
- **Safer multi-user access**: SQLite-backed allowlist with `admin` and `user` roles.
- **Practical file flow**: attachments go into a turn inbox, output files come back automatically.
- **Session memory without chaos**: import local Codex Desktop/CLI history by `cwd`.
- **Codex-first runtime**: Telecodex mirrors Codex sessions and settings instead of running its own local scheduler.

## 🧠 Core capabilities

### Conversation and session model

- Polls Telegram Bot API via `getUpdates`.
- Maintains one logical session per Telegram chat/topic pair.
- Queues turns per session and streams progress by editing Telegram messages in place.
- Supports `/new`, `/environments`, `/sessions`, `/use`, `/history`, `/status`, `/clear`, `/stop`, and per-session runtime settings.
- Can bind a Telegram topic to an existing Codex thread by thread id or `latest`.
- In the primary forum dashboard, environments are listed for import and topics are created on button click by default.

### Attachments and artifacts

- Accepts text, images, documents, audio, and video attachments.
- Stages incoming files under:

```text
<session cwd>/.telecodex/inbox/...
```

- Expects generated deliverables under:

```text
<session cwd>/.telecodex/turns/.../out
```

- Sends resulting files back to Telegram automatically.

### Audio transcription

- Optional audio transcription via `ffmpeg` + `transcribe-rs`.
- Auto-detects a local Handy Parakeet model directory when present.
- If transcription succeeds, the transcript is appended to the user prompt.

### Access control and safety

- SQLite-backed ACL with `allowed`, `admin`, and `user` role handling.
- Unauthorized access attempts are ignored and written to `audit_log`.
- Supports Codex runtime defaults for sandbox, approval policy, search mode, and writable directories.
- Supports headless Codex device login from Telegram via `/login` and `/logout`.
- If Codex is not logged in, Telecodex does not start turns or forward Codex-native slash commands; it asks the user to authenticate first.

### History and topic sync

- Reads local Codex history and imports existing sessions by `cwd`.
- Can browse final assistant messages from the selected Codex session with an interactive pager.
- Can sync forum topics from Codex Desktop and/or CLI history.
- Can target a dedicated Telegram forum chat for all new topics.
- Supports stale topic cleanup on a timer.

## 🏗️ How it works

```text
Telegram chat/topic
        ↓
     Telecodex
        ↓
 local codex CLI
        ↓
  workspace files
        ↓
 Telegram edits + artifacts
```

High-level flow:

1. Telegram sends updates through long polling.
2. Telecodex resolves the active session for the current chat/topic.
3. Incoming text and attachments are converted into a Codex turn request.
4. Codex runs locally in the configured workspace.
5. Progress is streamed back by editing Telegram messages.
6. Files produced in the turn output directory are uploaded back to Telegram.

## 🛠️ Command model

### Bridge-handled commands

| Command | Purpose |
| --- | --- |
| `/new [title]` | Start a fresh Codex session in the current topic/chat |
| `/topic [title]` | Create a new Telegram topic and copy the current environment into it |
| `/use <thread_id_prefix\|latest>` | Switch this Telegram session to an existing Codex thread |
| `/review [--uncommitted] [--base BRANCH] [--commit SHA] [--title TITLE] [prompt]` | Run `codex review`-style flows |
| `/login` | Start headless Codex device login, send a clickable auth link, and show the one-time code inline |
| `/logout` | Remove stored Codex credentials |
| `/cd <absolute_path>` | Change the session working directory |
| `/pwd` | Show the current working directory |
| `/environments` | Show importable Codex environments in the primary forum dashboard |
| `/sessions` | Show topic sessions in dashboard root, or Codex sessions for the current `cwd` inside a work topic |
| `/history` | Browse final assistant messages from the selected Codex session with an interactive pager |
| `/status` | Show the current Telegram session, selected Codex session, and runtime settings |
| `/stop` | Stop the active turn |
| `/model [model\|default\|-]` | Set or show the current model |
| `/think [minimal\|low\|medium\|high\|default\|-]` | Set or show reasoning effort |
| `/prompt [text\|clear\|default\|-]` | Set or clear the persistent session prompt |
| `/approval <never\|on-request\|untrusted>` | Set approval policy |
| `/sandbox <read-only\|workspace-write\|danger-full-access>` | Set sandbox mode |
| `/search <on\|off\|cached>` | Set search behavior |
| `/add-dir <absolute_path>` | Add a writable directory |
| `/limits` | Show Codex rate limits |
| `/copy` | Re-send the last assistant reply |
| `/clear` | Force a fresh session on the next turn |
| `/allow <tg_user_id>` | Admin: allow a Telegram user |
| `/deny <tg_user_id>` | Admin: deny a Telegram user |
| `/role <tg_user_id> <admin\|user>` | Admin: assign role |
| `/restart_bot` | Admin: restart the bot process |

### Forwarded to Codex as-is

`/help`, `/doctor`, `/prompts`, `/memory`, `/mentions`, `/init`, `/bug`, `/config`, `/compact`, `/agents`, `/diff`

These commands require an active Codex login. If the local Codex CLI is not authenticated yet, Telecodex will remind the user to run `/login` instead of forwarding them.

### Explicitly unsupported in Telegram

`/theme`, `/vim`, `/statusline`, `/browser`, `/ide`, `/notifications`, `/terminal-setup`

## ⚙️ Requirements

### Required

- Rust `1.85+`
- a working local `codex` CLI available on `PATH` or configured explicitly
- Telegram bot token
- [go-task](https://taskfile.dev/installation/)

### Optional but recommended

- `ffmpeg` for audio/video conversion
- a local Handy Parakeet model for speech transcription

## 🚀 Quick start

### 1. Clone and enter the repo

```bash
git clone https://github.com/Headcrab/telecodex.git
cd telecodex
```

### 2. Create the config

```bash
task init-config
```

This creates `telecodex.toml` from `telecodex.toml.example` if it does not exist.

### 3. Set your Telegram bot token

Set `TELEGRAM_BOT_TOKEN` in your environment before launch. Example:

```bash
export TELEGRAM_BOT_TOKEN="123456:replace-me"
```

### 4. Edit `telecodex.toml`

Minimal example:

```toml
db_path = "telecodex.sqlite3"
startup_admin_ids = [123456789]
poll_timeout_seconds = 30
edit_debounce_ms = 900
max_text_chunk = 3500
tmp_dir = "/absolute/path/to/telecodex/tmp"

[telegram]
bot_token_env = "TELEGRAM_BOT_TOKEN"
api_base = "https://api.telegram.org"
use_message_drafts = true

[codex]
binary = "codex"
default_cwd = "/absolute/path/to/telecodex"
default_model = "gpt-5.4"
default_reasoning_effort = "medium"
default_sandbox = "workspace-write"
default_approval = "never"
default_search_mode = "disabled"
import_desktop_history = true
import_cli_history = true
seed_workspaces = ["/absolute/path/to/workspace-a"]
default_add_dirs = ["/absolute/path/to/workspace"]
```

### 5. Run it

```bash
task run
```

### 6. Log in to Codex from Telegram

After the bot starts, open the Telegram chat with your bot and run:

```text
/login
```

Telecodex will start `codex login --device-auth`, send a clickable `auth.openai.com` link, show the one-time code inline in the message for quick copying, and post the result in chat when the login finishes.

## 🧩 Configuration notes

### Telegram

- `telegram.bot_token` or `telegram.bot_token_env` must be configured.
- `telegram.use_message_drafts = true` enables draft-style previews for private chats.
- `telegram.primary_forum_chat_id` is used by `/topic` to create topics in one dedicated forum.
- `telegram.auto_create_topics = false` keeps environment import manual; set it to `true` to auto-create missing forum topics from history.
- `telegram.forum_sync_topics_per_poll` throttles topic sync work.
- `telegram.stale_topic_days` + `telegram.stale_topic_action = "close"|"delete"` enable cleanup.

### Codex

- `codex.binary` can be a binary name or absolute path.
- `codex.default_cwd` must be an existing absolute directory.
- `codex.seed_workspaces` adds explicit workspace directories to `/environments` and forum sync, even before they have local Codex history.
- `codex.default_add_dirs` entries must also be absolute existing directories.
- `codex.import_desktop_history` and `codex.import_cli_history` control session import sources.
- `codex.default_search_mode` supports `disabled`, `live`, and `cached`.

### Environment variables

- `TELEGRAM_BOT_TOKEN`: Telegram Bot API token.
- `TELECODEX_RESTART_DELAY_MS`: optional startup delay before boot.

## 📁 Project layout

```text
src/
  app.rs            # main runtime loop and orchestration
  app/
    auth.rs         # Codex login/logout and device-code flow
    forum.rs        # forum/topic sync
    io.rs           # attachments and Telegram status delivery
    presentation.rs # formatting and keyboards
    support.rs      # shared helpers
    tests.rs        # app-level tests
    turns.rs        # turn execution pipeline
  commands.rs       # command parsing and help
  config.rs         # config loading and validation
  telegram.rs       # Telegram Bot API client
  store.rs          # SQLite persistence
  transcribe.rs     # optional audio transcription
```

## 🧪 Development

Build and run:

```bash
task build
task build-release
task run
task run-release
```

Validation:

```bash
task test
task verify
```

Available quality tasks:

```bash
task fmt
task fmt-check
task check
task clippy
```

Override config path when needed:

```bash
task run CONFIG=telecodex.toml
```

## 📌 Practical behavior notes

- Unauthorized updates are ignored and logged into `audit_log`.
- Existing Codex history can be auto-attached by `cwd` unless `/clear` was used.
- `/sessions` is contextual: in dashboard root it shows Telegram topic sessions, while inside a work topic it shows Codex sessions for the current `cwd`.
- `/history` browses final assistant messages from the selected Codex session, starts from the newest message, and wraps around at both ends.
- In the primary forum dashboard, `/environments` shows importable environments and creates topics only when you press the button unless `telegram.auto_create_topics = true`.
- `/new` now resets the Codex conversation inside the current topic and keeps the current environment/runtime settings.
- `/topic` is the explicit path for creating a new Telegram topic from the current environment.
- `/think` and `/prompt` persist for the current session and affect future turns.
- During active work the bot sends Telegram chat actions such as typing/upload indicators.
- In forum dashboard root, use `/environments` or `/sessions`; `/status`, `/history`, `/new`, and normal prompts are meant for an actual work topic.
- `/login` starts Codex device authentication in headless mode and sends a clickable auth link plus the one-time code inline in the message.
- If the device-code endpoint returns `429 Too Many Requests`, the bot reports that in chat and applies a short local backoff before the next `/login` attempt.
- After `/logout`, the bot stays responsive and keeps suggesting `/login` instead of going silent.
- `/status` is handled by Telecodex itself and shows the current Telegram session, selected Codex session, and runtime settings.
- If Codex is not logged in, Telecodex does not run turns and does not forward Codex-native slash commands; it asks the user to authenticate first.
- If a live prompt clearly asks for fresh information like "today", "latest", or "news", Telecodex can automatically switch that turn to live search.

## 📄 License

This project is licensed under the [MIT License](./LICENSE).

---

<div align="center">

Built for people who want Codex local, but reachable from Telegram.

</div>
