use super::*;
use std::{path::Path, process::Stdio};

pub(super) fn app_version_label() -> String {
    format!(
        "v{}.{}",
        env!("TELECODEX_APP_VERSION"),
        env!("TELECODEX_BUILD_NUMBER")
    )
}

pub(super) fn is_primary_forum_dashboard(
    config: &Config,
    chat: &crate::telegram::Chat,
    thread_id: Option<i64>,
) -> bool {
    config.telegram.primary_forum_chat_id == Some(chat.id)
        && chat.is_forum.unwrap_or(false)
        && thread_id.unwrap_or(0) == 0
}

pub(super) fn prefer_primary_environment_session(
    session: &crate::models::SessionRecord,
    environment_key: &Path,
) -> bool {
    normalize_path(session.cwd.clone()) == normalize_path(environment_key.to_path_buf())
}

pub(super) fn command_uses_session_context(parsed: &ParsedInput) -> bool {
    match parsed {
        ParsedInput::Forward(_) => true,
        ParsedInput::Bridge(command) => matches!(
            command,
            BridgeCommand::Topic { .. }
                | BridgeCommand::Review(_)
                | BridgeCommand::Cd { .. }
                | BridgeCommand::Pwd
                | BridgeCommand::Model { .. }
                | BridgeCommand::Think { .. }
                | BridgeCommand::Prompt { .. }
                | BridgeCommand::Approval { .. }
                | BridgeCommand::Sandbox { .. }
                | BridgeCommand::Search { .. }
                | BridgeCommand::AddDir { .. }
                | BridgeCommand::Limits
                | BridgeCommand::Copy
                | BridgeCommand::Clear
                | BridgeCommand::Unsupported { .. }
        ),
    }
}

pub(super) fn parsed_input_requires_codex_auth(parsed: &ParsedInput) -> bool {
    matches!(
        parsed,
        ParsedInput::Forward(_) | ParsedInput::Bridge(BridgeCommand::Review(_))
    )
}

pub(super) fn session_title_is_present(session: &crate::models::SessionRecord) -> bool {
    session
        .session_title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .is_some()
}

pub(super) fn derive_session_title_from_text(text: &str) -> Option<String> {
    let first_line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let collapsed = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    const LIMIT: usize = 48;
    if collapsed.chars().count() <= LIMIT {
        return Some(collapsed);
    }
    let truncated = collapsed.chars().take(LIMIT - 1).collect::<String>();
    Some(format!("{truncated}…"))
}

pub(super) fn active_session_state_key(user_id: i64, chat_id: i64) -> String {
    format!("active_session:{user_id}:{chat_id}")
}

pub(super) fn forum_sync_cooldown_key(chat_id: i64) -> String {
    format!("forum_sync_cooldown:{chat_id}")
}

pub(super) fn forum_sync_error_key(chat_id: i64) -> String {
    format!("forum_sync_error:{chat_id}")
}

pub(super) fn normalize_forum_sync_issue(issue: &str) -> String {
    issue
        .split(": retry after ")
        .next()
        .unwrap_or(issue)
        .trim()
        .to_string()
}

pub(super) fn forum_sync_cooldown_active(store: &Store, chat_id: i64) -> Result<bool> {
    let Some(value) = store.bot_state_value(&forum_sync_cooldown_key(chat_id))? else {
        return Ok(false);
    };
    let until = DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .ok();
    Ok(until.map(|until| until > Utc::now()).unwrap_or(false))
}

pub(super) fn active_session_identity(
    session_key: SessionKey,
    session: &crate::models::SessionRecord,
) -> String {
    format!(
        "{}:{}",
        session_key.thread_id,
        session.codex_thread_id.as_deref().unwrap_or("new")
    )
}

#[cfg(windows)]
pub(super) fn spawn_restarted_process() -> Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let cwd = std::env::current_dir().context("failed to resolve current working directory")?;
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .env("TELECODEX_RESTART_DELAY_MS", "2000")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().context("failed to spawn restarted bot")?;
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn spawn_restarted_process() -> Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let cwd = std::env::current_dir().context("failed to resolve current working directory")?;
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .env("TELECODEX_RESTART_DELAY_MS", "2000")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().context("failed to spawn restarted bot")?;
    Ok(())
}

pub(super) fn ensure_admin(user: &crate::models::UserRecord) -> Result<()> {
    if user.role != UserRole::Admin {
        bail!("admin role required");
    }
    Ok(())
}

pub(super) fn ensure_approval_policy(value: &str) -> Result<()> {
    match value {
        "never" | "on-request" | "untrusted" => Ok(()),
        _ => bail!("/approval <never|on-request|untrusted>"),
    }
}

pub(super) fn ensure_sandbox_mode(value: &str) -> Result<()> {
    match value {
        "read-only" | "workspace-write" | "danger-full-access" => Ok(()),
        _ => bail!("/sandbox <read-only|workspace-write|danger-full-access>"),
    }
}

pub(super) fn normalize_reasoning_effort(value: &str) -> Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "minimal" | "low" | "medium" | "high" => Ok(normalized),
        _ => bail!("/think <minimal|low|medium|high|default>"),
    }
}

pub(super) fn is_clear_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "-" | "clear" | "none" | "default"
    )
}

pub(super) fn validate_directory(path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        bail!("path must be absolute");
    }
    let path = normalize_path(
        fs::canonicalize(&path)
            .with_context(|| format!("failed to canonicalize {}", path.display()))?,
    );
    if !path.is_dir() {
        bail!("path is not a directory: {}", path.display());
    }
    Ok(path)
}

pub(super) fn normalize_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let raw = path.as_os_str().to_string_lossy();
        if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = raw.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

pub(super) fn telegram_retry_after(error: &anyhow::Error) -> Option<u64> {
    error
        .downcast_ref::<TelegramError>()
        .and_then(|telegram| telegram.retry_after)
}

pub(super) fn should_drop_telegram_rate_limited_send(error: &anyhow::Error) -> bool {
    telegram_retry_after(error).is_some()
}

pub(super) fn telegram_status(error: &anyhow::Error) -> Option<reqwest::StatusCode> {
    error
        .downcast_ref::<TelegramError>()
        .map(|telegram| telegram.status)
}

pub(super) fn is_message_not_modified(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<TelegramError>()
        .map(|telegram| telegram.description.contains("message is not modified"))
        .unwrap_or(false)
}

pub(super) fn is_message_thread_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<TelegramError>()
        .map(|telegram| telegram.description.contains("message thread not found"))
        .unwrap_or(false)
}

pub(super) fn is_invalid_forum_topic_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<TelegramError>()
        .map(|telegram| {
            telegram.description.contains("TOPIC_ID_INVALID")
                || telegram
                    .description
                    .contains("invalid forum topic identifier specified")
                || telegram.description.contains("message thread not found")
        })
        .unwrap_or(false)
}

pub(super) fn is_forum_topic_not_modified(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<TelegramError>()
        .map(|telegram| telegram.description.contains("TOPIC_NOT_MODIFIED"))
        .unwrap_or(false)
}

pub(super) fn auto_search_mode_for_prompt(prompt: &str) -> Option<crate::config::SearchMode> {
    let prompt = prompt.to_lowercase();
    let needs_live_search = [
        "what's new",
        "last day",
        "last 24 hours",
        "today",
        "news",
        "latest",
        "last 24 hours",
        "today",
        "current",
        "news",
    ]
    .iter()
    .any(|needle| prompt.contains(needle));

    if needs_live_search {
        Some(crate::config::SearchMode::Live)
    } else {
        None
    }
}
