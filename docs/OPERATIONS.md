# Telecodex Operations

## Start

```powershell
powershell -ExecutionPolicy Bypass -File D:\GitStorageG\RemoteCodex\scripts\Start-Telecodex.ps1
```

One-click alternative:

```text
D:\GitStorageG\RemoteCodex\Start-Telecodex.cmd
```

## Stop

```powershell
powershell -ExecutionPolicy Bypass -File D:\GitStorageG\RemoteCodex\scripts\Stop-Telecodex.ps1
```

One-click alternative:

```text
D:\GitStorageG\RemoteCodex\Stop-Telecodex.cmd
```

## Restart

```powershell
powershell -ExecutionPolicy Bypass -File D:\GitStorageG\RemoteCodex\scripts\Restart-Telecodex.ps1
```

One-click alternative:

```text
D:\GitStorageG\RemoteCodex\Restart-Telecodex.cmd
```

## Status

```powershell
powershell -ExecutionPolicy Bypass -File D:\GitStorageG\RemoteCodex\scripts\Status-Telecodex.ps1
```

One-click alternative:

```text
D:\GitStorageG\RemoteCodex\Status-Telecodex.cmd
```

## Rebuild

```powershell
powershell -ExecutionPolicy Bypass -File D:\GitStorageG\RemoteCodex\scripts\Build-Telecodex.ps1
```

## Change Telegram Token

1. Edit [`.env`](D:/GitStorageG/RemoteCodex/.env).
2. Replace `TELEGRAM_BOT_TOKEN=...`.
3. Restart Telecodex.

## Change Admin Whitelist

1. Edit [`telecodex.toml`](D:/GitStorageG/RemoteCodex/telecodex.toml).
2. Update `startup_admin_ids = [...]`.
3. Restart Telecodex.

## Add New Local Workspace

1. Add the absolute path to `codex.seed_workspaces` in [`telecodex.toml`](D:/GitStorageG/RemoteCodex/telecodex.toml).
2. If the workspace should always be writable by Codex when sandbox is `workspace-write`, also add its parent path to `codex.default_add_dirs`.
3. Restart Telecodex.

## Enable Forum Dashboard

Telegram-side requirements:

1. Create a `supergroup`.
2. Enable `Topics` in that group so it becomes a forum.
3. Add `@codex_ebot` to the group.
4. Promote the bot to admin with `Manage Topics`.
5. Send one message in the forum so the bot sees the chat.

Local binding:

```powershell
powershell -ExecutionPolicy Bypass -File D:\GitStorageG\RemoteCodex\scripts\Set-ForumDashboard.ps1 -ChatId <forum_chat_id>
```

If you want Telecodex to auto-create missing topics from local Codex history, use:

```powershell
powershell -ExecutionPolicy Bypass -File D:\GitStorageG\RemoteCodex\scripts\Set-ForumDashboard.ps1 -ChatId <forum_chat_id> -AutoCreateTopics
```

Then restart Telecodex.

## Logs

- stdout: [`logs/telecodex.stdout.log`](D:/GitStorageG/RemoteCodex/logs/telecodex.stdout.log)
- stderr: [`logs/telecodex.stderr.log`](D:/GitStorageG/RemoteCodex/logs/telecodex.stderr.log)

## Safe Defaults

- default sandbox: `read-only`
- default approval policy: `on-request`

For a writable session, switch inside Telegram:

```text
/sandbox workspace-write
/approval on-request
```
