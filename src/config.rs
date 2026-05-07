use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub telegram: TelegramConfig,
    pub codex: CodexConfig,
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    #[serde(default)]
    pub startup_admin_ids: Vec<i64>,
    #[serde(default = "default_poll_timeout_seconds")]
    pub poll_timeout_seconds: u32,
    #[serde(default = "default_edit_debounce_ms")]
    pub edit_debounce_ms: u64,
    #[serde(default = "default_max_text_chunk")]
    pub max_text_chunk: usize,
    pub tmp_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: Option<String>,
    pub bot_token_env: Option<String>,
    #[serde(default = "default_telegram_api_base")]
    pub api_base: String,
    #[serde(default = "default_true")]
    pub use_message_drafts: bool,
    pub primary_forum_chat_id: Option<i64>,
    #[serde(default)]
    pub auto_create_topics: bool,
    #[serde(default = "default_forum_sync_topics_per_poll")]
    pub forum_sync_topics_per_poll: usize,
    pub stale_topic_days: Option<i64>,
    #[serde(default)]
    pub stale_topic_action: StaleTopicAction,
    #[serde(default)]
    pub completion_notify_usernames: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum StaleTopicAction {
    #[default]
    None,
    Close,
    Delete,
}

impl StaleTopicAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Close => "close",
            Self::Delete => "delete",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CodexConfig {
    #[serde(default = "default_codex_binary")]
    pub binary: PathBuf,
    pub default_cwd: PathBuf,
    pub default_model: Option<String>,
    pub default_reasoning_effort: Option<String>,
    #[serde(default = "default_sandbox")]
    pub default_sandbox: String,
    #[serde(default = "default_approval")]
    pub default_approval: String,
    #[serde(default = "default_search_mode")]
    pub default_search_mode: SearchMode,
    #[serde(default)]
    pub default_add_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub seed_workspaces: Vec<PathBuf>,
    #[serde(default = "default_true")]
    pub import_desktop_history: bool,
    #[serde(default = "default_true")]
    pub import_cli_history: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Disabled,
    Live,
    Cached,
}

impl SearchMode {
    pub fn as_codex_value(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Live => "live",
            Self::Cached => "cached",
        }
    }
}

impl Default for SearchMode {
    fn default() -> Self {
        Self::Disabled
    }
}

impl Config {
    pub fn load(path: PathBuf) -> Result<Self> {
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let mut config: Config = toml::from_str(&raw)
            .with_context(|| format!("failed to parse config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&mut self) -> Result<()> {
        if !self.codex.default_cwd.is_absolute() {
            bail!("codex.default_cwd must be an absolute path");
        }
        if !self.codex.default_cwd.is_dir() {
            bail!(
                "codex.default_cwd must point to an existing directory: {}",
                self.codex.default_cwd.display()
            );
        }
        self.codex.default_cwd =
            normalize_path(fs::canonicalize(&self.codex.default_cwd).with_context(|| {
                format!(
                    "failed to canonicalize {}",
                    self.codex.default_cwd.display()
                )
            })?);

        for dir in &mut self.codex.default_add_dirs {
            if !dir.is_absolute() {
                bail!("codex.default_add_dirs entries must be absolute paths");
            }
            *dir = normalize_path(
                fs::canonicalize(&*dir)
                    .with_context(|| format!("failed to canonicalize {}", dir.display()))?,
            );
            if !dir.is_dir() {
                bail!(
                    "codex.default_add_dirs entry is not a directory: {}",
                    dir.display()
                );
            }
        }

        for workspace in &mut self.codex.seed_workspaces {
            if !workspace.is_absolute() {
                bail!("codex.seed_workspaces entries must be absolute paths");
            }
            *workspace = normalize_path(
                fs::canonicalize(&*workspace)
                    .with_context(|| format!("failed to canonicalize {}", workspace.display()))?,
            );
            if !workspace.is_dir() {
                bail!(
                    "codex.seed_workspaces entry is not a directory: {}",
                    workspace.display()
                );
            }
        }

        if let Some(tmp_dir) = &mut self.tmp_dir {
            if !tmp_dir.is_absolute() {
                bail!("tmp_dir must be an absolute path");
            }
            fs::create_dir_all(&tmp_dir)
                .with_context(|| format!("failed to create tmp_dir {}", tmp_dir.display()))?;
            *tmp_dir = normalize_path(
                fs::canonicalize(&tmp_dir)
                    .with_context(|| format!("failed to canonicalize {}", tmp_dir.display()))?,
            );
        }

        let token = self.telegram.resolve_token()?;
        if token.trim().is_empty() {
            bail!("telegram bot token is empty");
        }
        if let Some(days) = self.telegram.stale_topic_days {
            if days < 1 {
                bail!("telegram.stale_topic_days must be >= 1 when set");
            }
        }
        if self.telegram.forum_sync_topics_per_poll == 0 {
            bail!("telegram.forum_sync_topics_per_poll must be >= 1");
        }
        let mut completion_notify_usernames = Vec::new();
        for username in &self.telegram.completion_notify_usernames {
            let username = normalize_completion_notify_username(username)?;
            if !completion_notify_usernames
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&username))
            {
                completion_notify_usernames.push(username);
            }
        }
        self.telegram.completion_notify_usernames = completion_notify_usernames;

        if self.codex.binary.as_os_str().is_empty() {
            bail!("codex.binary must not be empty");
        }
        self.codex.binary = resolve_binary_path(&self.codex.binary)?;

        Ok(())
    }
}

impl TelegramConfig {
    pub fn resolve_token(&self) -> Result<String> {
        if let Some(token) = &self.bot_token {
            return Ok(token.clone());
        }
        if let Some(env_name) = &self.bot_token_env {
            return std::env::var(env_name)
                .with_context(|| format!("failed to read telegram token from env {env_name}"));
        }
        bail!("configure telegram.bot_token or telegram.bot_token_env")
    }
}

fn default_db_path() -> PathBuf {
    PathBuf::from("telecodex.sqlite3")
}

fn default_poll_timeout_seconds() -> u32 {
    30
}

fn default_edit_debounce_ms() -> u64 {
    900
}

fn default_max_text_chunk() -> usize {
    3500
}

fn default_telegram_api_base() -> String {
    "https://api.telegram.org".to_string()
}

fn default_true() -> bool {
    true
}

fn default_forum_sync_topics_per_poll() -> usize {
    2
}

fn default_codex_binary() -> PathBuf {
    PathBuf::from("codex")
}

fn default_sandbox() -> String {
    "workspace-write".to_string()
}

fn default_approval() -> String {
    "never".to_string()
}

fn default_search_mode() -> SearchMode {
    SearchMode::Disabled
}

fn normalize_completion_notify_username(input: &str) -> Result<String> {
    let username = input.trim().trim_start_matches('@');
    if username.is_empty() {
        bail!("telegram.completion_notify_usernames entries must not be empty");
    }
    if !(5..=32).contains(&username.len()) {
        bail!(
            "telegram.completion_notify_usernames entry `{}` must be 5..=32 characters",
            input
        );
    }
    if !username
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!(
            "telegram.completion_notify_usernames entry `{}` must contain only letters, digits, or underscores",
            input
        );
    }
    Ok(format!("@{username}"))
}

fn resolve_binary_path(input: &Path) -> Result<PathBuf> {
    if input.is_absolute() {
        if input.is_file() {
            return Ok(normalize_binary_path(input.to_path_buf()));
        }
        bail!("codex binary does not exist: {}", input.display());
    }

    let candidates = command_candidates(input);
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for candidate in &candidates {
            let joined = dir.join(candidate);
            if joined.is_file() {
                return Ok(normalize_binary_path(joined));
            }
        }
    }

    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let npm_dir = PathBuf::from(appdata).join("npm");
            for candidate in &candidates {
                let joined = npm_dir.join(candidate);
                if joined.is_file() {
                    return Ok(normalize_binary_path(joined));
                }
            }
        }
    }

    bail!(
        "failed to resolve executable `{}` from PATH",
        input.display()
    )
}

fn command_candidates(input: &Path) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut candidates = Vec::new();
        let stem = input.as_os_str().to_string_lossy();
        if !stem.contains('.') {
            for ext in [".cmd", ".exe", ".bat"] {
                candidates.push(PathBuf::from(format!("{stem}{ext}")));
            }
        }
        candidates.push(input.to_path_buf());
        candidates
    }
    #[cfg(not(windows))]
    {
        vec![input.to_path_buf()]
    }
}

fn normalize_path(path: PathBuf) -> PathBuf {
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

fn normalize_binary_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let normalized = normalize_path(path);
        let file_name = normalized
            .file_name()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase());
        if matches!(file_name.as_deref(), Some("codex.cmd" | "codex.bat")) {
            if let Some(exe) = find_vendored_codex_exe(&normalized) {
                return exe;
            }
        }
        return normalized;
    }

    #[allow(unreachable_code)]
    normalize_path(path)
}

#[cfg(windows)]
fn find_vendored_codex_exe(wrapper_path: &Path) -> Option<PathBuf> {
    let npm_dir = wrapper_path.parent()?;
    let candidate = npm_dir
        .join("node_modules")
        .join("@openai")
        .join("codex")
        .join("node_modules")
        .join("@openai")
        .join("codex-win32-x64")
        .join("vendor")
        .join("x86_64-pc-windows-msvc")
        .join("codex")
        .join("codex.exe");
    if candidate.is_file() {
        Some(normalize_path(candidate))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_completion_notify_usernames() {
        assert_eq!(
            normalize_completion_notify_username(" sample_user ")
                .unwrap()
                .as_str(),
            "@sample_user"
        );
        assert_eq!(
            normalize_completion_notify_username("@sample_user")
                .unwrap()
                .as_str(),
            "@sample_user"
        );
    }

    #[test]
    fn rejects_invalid_completion_notify_usernames() {
        assert!(normalize_completion_notify_username("@bad-name").is_err());
        assert!(normalize_completion_notify_username("@abc").is_err());
    }
}
