use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::SearchMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    pub chat_id: i64,
    pub thread_id: i64,
}

impl SessionKey {
    pub fn new(chat_id: i64, thread_id: Option<i64>) -> Self {
        Self {
            chat_id,
            thread_id: thread_id.unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserRole {
    Admin,
    User,
}

impl UserRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::User => "user",
        }
    }
}

impl TryFrom<&str> for UserRole {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "admin" => Ok(Self::Admin),
            "user" => Ok(Self::User),
            other => anyhow::bail!("unsupported role {other}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UserRecord {
    pub tg_user_id: i64,
    pub role: UserRole,
    pub allowed: bool,
}

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: i64,
    pub key: SessionKey,
    pub session_title: Option<String>,
    pub codex_thread_id: Option<String>,
    pub force_fresh_thread: bool,
    pub updated_at: String,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub session_prompt: Option<String>,
    pub sandbox_mode: String,
    pub approval_policy: String,
    pub search_mode: SearchMode,
    pub add_dirs: Vec<PathBuf>,
    pub busy: bool,
}

#[derive(Debug, Clone)]
pub struct TurnRequest {
    pub session_key: SessionKey,
    pub from_user_id: i64,
    pub prompt: String,
    pub runtime_instructions: Option<String>,
    pub attachments: Vec<LocalAttachment>,
    pub review_mode: Option<ReviewRequest>,
    pub override_search_mode: Option<SearchMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub base: Option<String>,
    pub commit: Option<String>,
    pub uncommitted: bool,
    pub title: Option<String>,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TelegramMessageRef {
    pub chat_id: i64,
    pub message_id: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Text,
    Audio,
    Voice,
    Video,
    Document,
}

#[derive(Debug, Clone)]
pub struct LocalAttachment {
    pub path: PathBuf,
    pub file_name: String,
    pub mime_type: Option<String>,
    pub kind: AttachmentKind,
    pub transcript: Option<AttachmentTranscript>,
}

#[derive(Debug, Clone)]
pub struct AttachmentTranscript {
    #[allow(dead_code)]
    pub engine: String,
    pub text: String,
}

impl TurnRequest {
    pub fn image_paths(&self) -> Vec<PathBuf> {
        self.attachments
            .iter()
            .filter(|attachment| attachment.kind == AttachmentKind::Image)
            .map(|attachment| attachment.path.clone())
            .collect()
    }
}
