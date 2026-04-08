# Troubleshooting

## Bot Does Not Reply

1. Check process status:

```powershell
powershell -ExecutionPolicy Bypass -File D:\GitStorageG\RemoteCodex\scripts\Status-Telecodex.ps1
```

2. Check logs:

- [`logs/telecodex.stdout.log`](D:/GitStorageG/RemoteCodex/logs/telecodex.stdout.log)
- [`logs/telecodex.stderr.log`](D:/GitStorageG/RemoteCodex/logs/telecodex.stderr.log)

3. Verify the bot token in [`.env`](D:/GitStorageG/RemoteCodex/.env).

## Build Fails After Update

Rebuild with the bundled launcher:

```powershell
powershell -ExecutionPolicy Bypass -File D:\GitStorageG\RemoteCodex\scripts\Build-Telecodex.ps1
```

If `ort-sys` link errors mention unresolved `__std_*` symbols, the build is using the wrong MSVC STL. Re-run the build only through the provided script.

## Telegram User Is Rejected

The user is not in `startup_admin_ids`. Update [`telecodex.toml`](D:/GitStorageG/RemoteCodex/telecodex.toml) and restart.

## Codex Is Not Authenticated

Check:

```powershell
codex login status
```

If needed, authenticate the local Codex CLI first, then restart Telecodex.

## Workspace Is Missing

- Add the absolute path to `codex.seed_workspaces`
- Restart Telecodex
- In Telegram, use `/environments` or `/cd <absolute path>`
