# <div align="center">🤖 Telecodex</div>

<div align="center">
  <strong>Telegram как удалённый, topic-aware интерфейс для локального Codex CLI.</strong>
</div>

<div align="center">
  Long polling. Постоянные сессии. Вложения. Синхронизация рабочих окружений. SQLite ACL.
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

[🇬🇧 English README](./README.md) • [📄 Лицензия MIT](./LICENSE)

</div>

---

## ✨ Что это такое

**Telecodex** это мост на Rust, который связывает локальный `codex` CLI с Telegram.

Он превращает Telegram-чаты и forum topics в удобные удалённые рабочие сессии, где можно:

- общаться с Codex с телефона или десктопного Telegram,
- держать отдельные сессии на каждый чат или topic,
- переключаться между существующими Codex thread'ами,
- отправлять файлы и медиа прямо в ход,
- получать потоковый прогресс и готовые артефакты обратно в Telegram.

Без webhook-инфры. Без браузерной прослойки. Без облачного релея между Telegram и локальным Codex.

## 🔥 Зачем это нужно

- **Удалённый доступ без боли**: Telegram становится интерфейсом, а Codex остаётся локальным.
- **Сессии по топикам**: каждый forum topic может жить как отдельная рабочая среда.
- **Нормальный контроль доступа**: allowlist и роли `admin` / `user` хранятся в SQLite.
- **Адекватный файловый цикл**: вложения попадают во входящую папку хода, выходные файлы возвращаются автоматически.
- **Память без каши**: можно подтягивать локальную историю Codex Desktop/CLI по `cwd`.
- **Codex-first runtime**: Telecodex зеркалит сессии и настройки Codex, а не запускает свой локальный scheduler.

## 🧠 Основные возможности

### Диалоги и модель сессий

- Поллит Telegram Bot API через `getUpdates`.
- Держит одну логическую сессию на пару chat/topic.
- Ставит ходы в очередь по сессии и стримит прогресс через редактирование сообщений.
- Поддерживает `/new`, `/environments`, `/sessions`, `/use`, `/history`, `/status`, `/clear`, `/stop` и настройки рантайма на уровне сессии.
- Может привязать Telegram topic к существующему Codex thread по id или `latest`.
- В primary forum dashboard окружения показываются для импорта, а topic создаётся по нажатию кнопки по умолчанию.

### Вложения и артефакты

- Принимает текст, картинки, документы, аудио и видео.
- Складывает входящие файлы сюда:

```text
<session cwd>/.telecodex/inbox/...
```

- Ждёт финальные артефакты здесь:

```text
<session cwd>/.telecodex/turns/.../out
```

- Автоматически отправляет получившиеся файлы обратно в Telegram.

### Расшифровка аудио

- Опциональная транскрибация через `ffmpeg` + `transcribe-rs`.
- Автоматически ищет локальную модель Handy Parakeet, если она есть.
- Если транскрипция успешна, текст добавляется к пользовательскому промпту.

### Контроль доступа и безопасность

- SQLite ACL с флагом `allowed` и ролями `admin` / `user`.
- Неавторизованные попытки игнорируются и пишутся в `audit_log`.
- Поддерживаются дефолты Codex для sandbox, approval policy, search mode и writable directories.
- Поддерживается headless device login в Codex через Telegram-команды `/login` и `/logout`.
- Если Codex не залогинен, Telecodex не запускает ходы и не пробрасывает Codex-native slash-команды, а просит сначала авторизоваться.

### История и синхронизация topic'ов

- Читает локальную историю Codex и импортирует существующие сессии по `cwd`.
- Может листать итоговые сообщения ассистента из выбранной Codex-сессии через интерактивный pager.
- Может синхронизировать forum topics из истории Codex Desktop и/или CLI.
- Умеет направлять создание новых topic'ов в отдельный Telegram forum chat.
- Поддерживает очистку старых topic'ов по таймеру.

## 🏗️ Как это работает

```text
Telegram chat/topic
        ↓
     Telecodex
        ↓
 local codex CLI
        ↓
  файлы workspace
        ↓
 Telegram edits + артефакты
```

Верхнеуровневый поток такой:

1. Telegram присылает апдейты через long polling.
2. Telecodex определяет активную сессию для текущего чата/topic.
3. Текст и вложения превращаются в запрос на ход для Codex.
4. Codex выполняется локально в настроенном workspace.
5. Прогресс стримится обратно через редактирование Telegram-сообщений.
6. Файлы из выходной директории хода отправляются пользователю.

## 🛠️ Модель команд

### Команды, которые обрабатывает мост

| Команда | Назначение |
| --- | --- |
| `/new [title]` | Начать свежую Codex-сессию в текущем topic/chat |
| `/topic [title]` | Создать новый Telegram topic и скопировать в него текущее окружение |
| `/use <thread_id_prefix\|latest>` | Переключить Telegram-сессию на существующий Codex thread |
| `/review [--uncommitted] [--base BRANCH] [--commit SHA] [--title TITLE] [prompt]` | Запустить сценарий review |
| `/login` | Запустить headless device login, прислать кликабельную auth-ссылку и показать одноразовый код inline |
| `/logout` | Удалить сохранённые креды Codex |
| `/cd <absolute_path>` | Поменять рабочую директорию сессии |
| `/pwd` | Показать текущую рабочую директорию |
| `/environments` | Показать доступные для импорта Codex environments в primary forum dashboard |
| `/sessions` | Показать topic-сессии в корне dashboard или Codex-сессии для текущего `cwd` внутри рабочего topic |
| `/history` | Листать итоговые сообщения ассистента из выбранной Codex-сессии через интерактивный pager |
| `/status` | Показать текущую Telegram-сессию, выбранную Codex-сессию и runtime-настройки |
| `/stop` | Остановить активный ход |
| `/model [model\|default\|-]` | Поставить или показать модель |
| `/think [minimal\|low\|medium\|high\|default\|-]` | Поставить или показать reasoning effort |
| `/prompt [text\|clear\|default\|-]` | Поставить или очистить постоянный session prompt |
| `/approval <never\|on-request\|untrusted>` | Поставить approval policy |
| `/sandbox <read-only\|workspace-write\|danger-full-access>` | Поставить sandbox mode |
| `/search <on\|off\|cached>` | Поставить режим поиска |
| `/add-dir <absolute_path>` | Добавить writable directory |
| `/limits` | Показать лимиты Codex |
| `/copy` | Ещё раз отправить последний ответ ассистента |
| `/clear` | На следующем ходе начать свежую сессию |
| `/allow <tg_user_id>` | Admin: разрешить пользователя |
| `/deny <tg_user_id>` | Admin: запретить пользователя |
| `/role <tg_user_id> <admin\|user>` | Admin: назначить роль |
| `/restart_bot` | Admin: перезапустить процесс бота |

### Команды, которые пробрасываются в Codex как есть

`/help`, `/doctor`, `/prompts`, `/memory`, `/mentions`, `/init`, `/bug`, `/config`, `/compact`, `/agents`, `/diff`

Эти команды требуют активной авторизации в Codex. Если локальный Codex CLI ещё не залогинен, Telecodex не будет их пробрасывать и попросит выполнить `/login`.

### Команды, которые в Telegram сознательно не поддерживаются

`/theme`, `/vim`, `/statusline`, `/browser`, `/ide`, `/notifications`, `/terminal-setup`

## ⚙️ Требования

### Обязательные

- Rust `1.85+`
- рабочий локальный `codex` CLI в `PATH` или явно прописанный в конфиге
- Telegram bot token
- [go-task](https://taskfile.dev/installation/)

### Опциональные, но полезные

- `ffmpeg` для конвертации аудио/видео
- локальная модель Handy Parakeet для транскрибации речи

## 🚀 Быстрый старт

### 1. Клонировать репозиторий

```bash
git clone https://github.com/Headcrab/telecodex.git
cd telecodex
```

### 2. Создать конфиг

```bash
task init-config
```

Эта команда создаёт `telecodex.toml` из `telecodex.toml.example`, если файла ещё нет.

### 3. Выставить Telegram bot token

Перед запуском задай `TELEGRAM_BOT_TOKEN` в окружении. Пример:

```bash
export TELEGRAM_BOT_TOKEN="123456:replace-me"
```

### 4. Отредактировать `telecodex.toml`

Минимальный пример:

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
default_add_dirs = ["/absolute/path/to/workspace"]
```

### 5. Запустить

```bash
task run
```

### 6. Войти в Codex из Telegram

После старта бота открой чат с ботом в Telegram и выполни:

```text
/login
```

Telecodex запустит `codex login --device-auth`, пришлёт кликабельную ссылку на `auth.openai.com`, покажет одноразовый код inline в сообщении для быстрого копирования, а затем напишет результат в чат после завершения логина.

## 🧩 Пояснения по конфигу

### Telegram

- `telegram.bot_token` или `telegram.bot_token_env` должны быть заданы.
- `telegram.use_message_drafts = true` включает draft-подобные превью в приватных чатах.
- `telegram.primary_forum_chat_id` используется командой `/topic`, чтобы создавать topic'и в одном выделенном форуме.
- `telegram.auto_create_topics = false` оставляет импорт окружений ручным; поставь `true`, если хочешь автосоздание недостающих topic'ов из истории.
- `telegram.forum_sync_topics_per_poll` ограничивает интенсивность topic sync.
- `telegram.stale_topic_days` + `telegram.stale_topic_action = "close"|"delete"` включают очистку старых topic'ов.

### Codex

- `codex.binary` может быть именем бинаря или абсолютным путём.
- `codex.default_cwd` обязан быть существующей абсолютной директорией.
- `codex.default_add_dirs` тоже должны быть абсолютными существующими директориями.
- `codex.import_desktop_history` и `codex.import_cli_history` управляют источниками импорта сессий.
- `codex.default_search_mode` поддерживает `disabled`, `live` и `cached`.

### Переменные окружения

- `TELEGRAM_BOT_TOKEN`: токен Telegram Bot API.
- `TELECODEX_RESTART_DELAY_MS`: опциональная задержка перед стартом процесса.

## 📁 Структура проекта

```text
src/
  app.rs            # главный runtime и оркестрация
  app/
    auth.rs         # login/logout Codex и device-code flow
    forum.rs        # forum/topic sync
    io.rs           # вложения и доставка статусных сообщений
    presentation.rs # форматирование и клавиатуры
    support.rs      # общие helper'ы
    tests.rs        # app-level тесты
    turns.rs        # pipeline выполнения ходов
  commands.rs       # парсинг команд и help
  config.rs         # загрузка и валидация конфига
  telegram.rs       # клиент Telegram Bot API
  store.rs          # SQLite persistence
  transcribe.rs     # опциональная транскрибация аудио
```

## 🧪 Разработка

Сборка и запуск:

```bash
task build
task build-release
task run
task run-release
```

Проверки:

```bash
task test
task verify
```

Доступные quality-задачи:

```bash
task fmt
task fmt-check
task check
task clippy
```

Если нужен другой конфиг:

```bash
task run CONFIG=telecodex.toml
```

## 📌 Практические замечания

- Неавторизованные апдейты игнорируются и пишутся в `audit_log`.
- Существующая история Codex может автоматически подтягиваться по `cwd`, если не был вызван `/clear`.
- `/sessions` контекстная команда: в корне dashboard она показывает Telegram topic-сессии, а внутри рабочего topic показывает Codex-сессии для текущего `cwd`.
- `/history` листает итоговые сообщения ассистента из выбранной Codex-сессии, начинает с самого нового и циклически переходит через края списка.
- В primary forum dashboard `/environments` показывает доступные для импорта окружения и создаёт topic только по нажатию кнопки, если не включён `telegram.auto_create_topics = true`.
- `/new` теперь сбрасывает Codex-разговор внутри текущего topic и сохраняет текущее окружение и runtime-настройки.
- `/topic` теперь является явным способом создать новый Telegram topic из текущего окружения.
- `/think` и `/prompt` сохраняются на уровне сессии и влияют на следующие ходы.
- Во время работы бот шлёт Telegram chat actions вроде typing/upload.
- В корне forum dashboard надо использовать `/environments` или `/sessions`; `/status`, `/history`, `/new` и обычные промпты предназначены для рабочего topic.
- `/login` запускает headless device authentication и присылает кликабельную auth-ссылку и одноразовый код inline в сообщении.
- Если device-code endpoint отвечает `429 Too Many Requests`, бот пишет об этом в чат и включает короткий локальный backoff перед следующей попыткой `/login`.
- После `/logout` бот остаётся доступным и продолжает подсказывать `/login`, а не замолкает.
- `/status` обрабатывается самим Telecodex и показывает текущую Telegram-сессию, выбранную Codex-сессию и runtime-настройки.
- Если Codex не залогинен, Telecodex не запускает ходы и не пробрасывает Codex-native slash-команды, а просит сначала авторизоваться.
- Если в промпте явно просят свежую инфу вроде "today", "latest" или "news", Telecodex может автоматически включить live search для этого хода.

## 📄 Лицензия

Проект распространяется под [MIT License](./LICENSE).

---

<div align="center">

Сделано для тех, кому нужен локальный Codex, но с доступом через Telegram.

</div>
