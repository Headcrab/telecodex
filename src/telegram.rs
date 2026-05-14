use std::{
    collections::HashMap,
    error::Error,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::Mutex;
use tokio::time::sleep;

#[derive(Clone)]
pub struct TelegramClient {
    http: reqwest::Client,
    token: String,
    api_base: String,
    outbound: Arc<OutboundRateLimiter>,
}

const TELEGRAM_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const TELEGRAM_GET_UPDATES_GRACE: Duration = Duration::from_secs(15);
const TELEGRAM_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const TELEGRAM_UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);
pub const TELEGRAM_GROUP_OUTBOUND_INTERVAL_MS: u64 = 3_500;
pub const TELEGRAM_PRIVATE_OUTBOUND_INTERVAL_MS: u64 = 1_000;
const TELEGRAM_GLOBAL_OUTBOUND_INTERVAL_MS: u64 = 40;

impl TelegramClient {
    pub fn new(token: String, api_base: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            token,
            api_base: api_base.trim_end_matches('/').to_string(),
            outbound: Arc::new(OutboundRateLimiter::default()),
        }
    }

    pub async fn get_me(&self) -> Result<User> {
        self.post::<(), User>("getMe", None).await
    }

    pub async fn get_updates(&self, offset: Option<i64>, timeout: u32) -> Result<Vec<Update>> {
        #[derive(Serialize)]
        struct Payload {
            offset: Option<i64>,
            timeout: u32,
            allowed_updates: Vec<&'static str>,
        }

        self.post_with_timeout(
            "getUpdates",
            Some(&Payload {
                offset,
                timeout,
                allowed_updates: vec!["message", "callback_query"],
            }),
            Duration::from_secs(timeout as u64).saturating_add(TELEGRAM_GET_UPDATES_GRACE),
        )
        .await
    }

    pub async fn set_my_commands(&self, commands: &[BotCommand]) -> Result<()> {
        #[derive(Serialize)]
        struct Payload<'a> {
            commands: &'a [BotCommand],
        }

        let _: bool = self
            .post("setMyCommands", Some(&Payload { commands }))
            .await?;
        Ok(())
    }

    pub async fn send_message(&self, request: SendMessage) -> Result<Message> {
        let chat_id = request.chat_id;
        self.post_outbound(chat_id, "sendMessage", Some(&request))
            .await
    }

    pub async fn send_chat_action(
        &self,
        chat_id: i64,
        message_thread_id: Option<i64>,
        action: ChatAction,
    ) -> Result<bool> {
        #[derive(Serialize)]
        struct Payload<'a> {
            chat_id: i64,
            message_thread_id: Option<i64>,
            action: &'a str,
        }

        self.post(
            "sendChatAction",
            Some(&Payload {
                chat_id,
                message_thread_id,
                action: action.as_str(),
            }),
        )
        .await
    }

    pub async fn edit_message_text(&self, request: EditMessageText) -> Result<Message> {
        let chat_id = request.chat_id;
        self.post_outbound(chat_id, "editMessageText", Some(&request))
            .await
    }

    pub async fn answer_callback_query(&self, callback_query_id: &str) -> Result<bool> {
        #[derive(Serialize)]
        struct Payload<'a> {
            callback_query_id: &'a str,
        }

        self.post("answerCallbackQuery", Some(&Payload { callback_query_id }))
            .await
    }

    pub async fn send_photo(
        &self,
        chat_id: i64,
        message_thread_id: Option<i64>,
        path: &std::path::Path,
        file_name: &str,
        mime_type: Option<&str>,
    ) -> Result<Message> {
        self.post_multipart_message(
            "sendPhoto",
            chat_id,
            message_thread_id,
            "photo",
            path,
            file_name,
            mime_type,
        )
        .await
    }

    pub async fn send_document(
        &self,
        chat_id: i64,
        message_thread_id: Option<i64>,
        path: &std::path::Path,
        file_name: &str,
        mime_type: Option<&str>,
    ) -> Result<Message> {
        self.post_multipart_message(
            "sendDocument",
            chat_id,
            message_thread_id,
            "document",
            path,
            file_name,
            mime_type,
        )
        .await
    }

    pub async fn send_audio(
        &self,
        chat_id: i64,
        message_thread_id: Option<i64>,
        path: &std::path::Path,
        file_name: &str,
        mime_type: Option<&str>,
    ) -> Result<Message> {
        self.post_multipart_message(
            "sendAudio",
            chat_id,
            message_thread_id,
            "audio",
            path,
            file_name,
            mime_type,
        )
        .await
    }

    pub async fn send_video(
        &self,
        chat_id: i64,
        message_thread_id: Option<i64>,
        path: &std::path::Path,
        file_name: &str,
        mime_type: Option<&str>,
    ) -> Result<Message> {
        self.post_multipart_message(
            "sendVideo",
            chat_id,
            message_thread_id,
            "video",
            path,
            file_name,
            mime_type,
        )
        .await
    }

    pub async fn create_forum_topic(&self, chat_id: i64, name: &str) -> Result<ForumTopic> {
        #[derive(Serialize)]
        struct Payload<'a> {
            chat_id: i64,
            name: &'a str,
        }

        self.post_outbound(
            chat_id,
            "createForumTopic",
            Some(&Payload { chat_id, name }),
        )
        .await
    }

    pub async fn close_forum_topic(&self, chat_id: i64, message_thread_id: i64) -> Result<bool> {
        #[derive(Serialize)]
        struct Payload {
            chat_id: i64,
            message_thread_id: i64,
        }

        self.post_outbound(
            chat_id,
            "closeForumTopic",
            Some(&Payload {
                chat_id,
                message_thread_id,
            }),
        )
        .await
    }

    pub async fn delete_forum_topic(&self, chat_id: i64, message_thread_id: i64) -> Result<bool> {
        #[derive(Serialize)]
        struct Payload {
            chat_id: i64,
            message_thread_id: i64,
        }

        self.post_outbound(
            chat_id,
            "deleteForumTopic",
            Some(&Payload {
                chat_id,
                message_thread_id,
            }),
        )
        .await
    }

    pub async fn edit_forum_topic(
        &self,
        chat_id: i64,
        message_thread_id: i64,
        name: &str,
    ) -> Result<bool> {
        #[derive(Serialize)]
        struct Payload<'a> {
            chat_id: i64,
            message_thread_id: i64,
            name: &'a str,
        }

        self.post_outbound(
            chat_id,
            "editForumTopic",
            Some(&Payload {
                chat_id,
                message_thread_id,
                name,
            }),
        )
        .await
    }

    pub async fn send_message_draft(&self, request: SendMessageDraft) -> Result<bool> {
        let chat_id = request.chat_id;
        self.post_outbound(chat_id, "sendMessageDraft", Some(&request))
            .await
    }

    pub async fn get_file(&self, file_id: &str) -> Result<File> {
        #[derive(Serialize)]
        struct Payload<'a> {
            file_id: &'a str,
        }

        self.post("getFile", Some(&Payload { file_id })).await
    }

    pub async fn download_file(&self, file_path: &str) -> Result<Vec<u8>> {
        let url = format!("{}/file/bot{}/{}", self.api_base, self.token, file_path);
        let response = self
            .http
            .get(url)
            .timeout(TELEGRAM_DOWNLOAD_TIMEOUT)
            .send()
            .await
            .context("telegram getFile download failed")?;
        let status = response.status();
        if !status.is_success() {
            bail!("telegram file download failed with status {status}");
        }
        Ok(response.bytes().await?.to_vec())
    }

    async fn post<T, R>(&self, method: &str, payload: Option<&T>) -> Result<R>
    where
        T: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        self.post_with_timeout(method, payload, TELEGRAM_REQUEST_TIMEOUT)
            .await
    }

    async fn post_outbound<T, R>(
        &self,
        chat_id: i64,
        method: &str,
        payload: Option<&T>,
    ) -> Result<R>
    where
        T: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        self.outbound.wait(chat_id).await;
        let result = self.post(method, payload).await;
        if let Err(error) = &result {
            self.outbound.note_retry_after(chat_id, error).await;
        }
        result
    }

    async fn post_with_timeout<T, R>(
        &self,
        method: &str,
        payload: Option<&T>,
        timeout: Duration,
    ) -> Result<R>
    where
        T: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let url = format!("{}/bot{}/{}", self.api_base, self.token, method);
        let mut request = self.http.post(url);
        if let Some(payload) = payload {
            request = request.json(payload);
        }
        request = request.timeout(timeout);

        let response = request
            .send()
            .await
            .with_context(|| format!("telegram {method} request failed"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .with_context(|| format!("telegram {method} response body failed"))?;

        if !status.is_success() {
            let parsed = serde_json::from_str::<ApiResponse<R>>(&body).ok();
            if let Some(parsed) = parsed {
                if let Some(parameters) = parsed.parameters {
                    return Err(TelegramError {
                        status,
                        description: parsed
                            .description
                            .unwrap_or_else(|| "telegram api error".to_string()),
                        retry_after: parameters.retry_after,
                    }
                    .into());
                }
            }
            return Err(TelegramError {
                status,
                description: body,
                retry_after: None,
            }
            .into());
        }

        let parsed: ApiResponse<R> = serde_json::from_str(&body)
            .with_context(|| format!("telegram {method} JSON decode failed"))?;
        if !parsed.ok {
            return Err(TelegramError {
                status,
                description: parsed
                    .description
                    .unwrap_or_else(|| "telegram api error".to_string()),
                retry_after: parsed
                    .parameters
                    .and_then(|parameters| parameters.retry_after),
            }
            .into());
        }

        parsed
            .result
            .ok_or_else(|| anyhow::anyhow!("telegram {method} returned ok without result"))
    }

    #[allow(clippy::too_many_arguments)]
    async fn post_multipart_message(
        &self,
        method: &str,
        chat_id: i64,
        message_thread_id: Option<i64>,
        file_field: &str,
        path: &std::path::Path,
        file_name: &str,
        mime_type: Option<&str>,
    ) -> Result<Message> {
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("failed to read upload file {}", path.display()))?;
        self.outbound.wait(chat_id).await;
        let url = format!("{}/bot{}/{}", self.api_base, self.token, method);
        let part = if let Some(mime_type) = mime_type {
            match Part::bytes(bytes.clone())
                .file_name(file_name.to_string())
                .mime_str(mime_type)
            {
                Ok(part) => part,
                Err(_) => Part::bytes(bytes).file_name(file_name.to_string()),
            }
        } else {
            Part::bytes(bytes).file_name(file_name.to_string())
        };

        let mut form = Form::new()
            .text("chat_id", chat_id.to_string())
            .part(file_field.to_string(), part);
        if let Some(thread_id) = message_thread_id {
            form = form.text("message_thread_id", thread_id.to_string());
        }

        let response = self
            .http
            .post(url)
            .multipart(form)
            .timeout(TELEGRAM_UPLOAD_TIMEOUT)
            .send()
            .await
            .with_context(|| format!("telegram {method} multipart request failed"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .with_context(|| format!("telegram {method} response body failed"))?;

        if !status.is_success() {
            let parsed = serde_json::from_str::<ApiResponse<Message>>(&body).ok();
            if let Some(parsed) = parsed {
                if let Some(parameters) = parsed.parameters {
                    if let Some(retry_after) = parameters.retry_after {
                        self.outbound.backoff(chat_id, retry_after).await;
                    }
                    return Err(TelegramError {
                        status,
                        description: parsed
                            .description
                            .unwrap_or_else(|| "telegram api error".to_string()),
                        retry_after: parameters.retry_after,
                    }
                    .into());
                }
            }
            return Err(TelegramError {
                status,
                description: body,
                retry_after: None,
            }
            .into());
        }

        let parsed: ApiResponse<Message> = serde_json::from_str(&body)
            .with_context(|| format!("telegram {method} JSON decode failed"))?;
        if !parsed.ok {
            if let Some(retry_after) = parsed
                .parameters
                .as_ref()
                .and_then(|parameters| parameters.retry_after)
            {
                self.outbound.backoff(chat_id, retry_after).await;
            }
            return Err(TelegramError {
                status,
                description: parsed
                    .description
                    .unwrap_or_else(|| "telegram api error".to_string()),
                retry_after: parsed
                    .parameters
                    .and_then(|parameters| parameters.retry_after),
            }
            .into());
        }

        parsed
            .result
            .ok_or_else(|| anyhow::anyhow!("telegram {method} returned ok without result"))
    }
}

#[derive(Debug)]
struct OutboundRateLimiter {
    state: Mutex<OutboundLimiterState>,
}

impl Default for OutboundRateLimiter {
    fn default() -> Self {
        Self {
            state: Mutex::new(OutboundLimiterState::default()),
        }
    }
}

impl OutboundRateLimiter {
    async fn wait(&self, chat_id: i64) {
        loop {
            let next = {
                let mut state = self.state.lock().await;
                state.acquire(chat_id, Instant::now())
            };
            match next {
                Ok(()) => return,
                Err(next_at) => {
                    let wait = next_at.saturating_duration_since(Instant::now());
                    if !wait.is_zero() {
                        sleep(wait).await;
                    }
                }
            }
        }
    }

    async fn note_retry_after(&self, chat_id: i64, error: &anyhow::Error) {
        if let Some(retry_after) = telegram_retry_after(error) {
            self.backoff(chat_id, retry_after).await;
        }
    }

    async fn backoff(&self, chat_id: i64, retry_after: u64) {
        let mut state = self.state.lock().await;
        state.backoff(chat_id, Instant::now(), retry_after);
    }
}

#[derive(Debug, Default)]
struct OutboundLimiterState {
    next_global_at: Option<Instant>,
    next_chat_at: HashMap<i64, Instant>,
}

impl OutboundLimiterState {
    fn acquire(&mut self, chat_id: i64, now: Instant) -> std::result::Result<(), Instant> {
        let slot = self.next_available_at(chat_id, now);
        if slot > now {
            return Err(slot);
        }
        self.next_global_at =
            Some(now + Duration::from_millis(TELEGRAM_GLOBAL_OUTBOUND_INTERVAL_MS));
        self.next_chat_at
            .insert(chat_id, now + outbound_interval_for_chat(chat_id));
        Ok(())
    }

    fn next_available_at(&self, chat_id: i64, now: Instant) -> Instant {
        let next_global = self.next_global_at.unwrap_or(now);
        let next_chat = self.next_chat_at.get(&chat_id).copied().unwrap_or(now);
        next_global.max(next_chat).max(now)
    }

    fn backoff(&mut self, chat_id: i64, now: Instant, retry_after: u64) {
        let until = now + Duration::from_secs(retry_after.saturating_add(1));
        self.next_global_at = Some(self.next_global_at.unwrap_or(now).max(until));
        let next_chat = self.next_chat_at.entry(chat_id).or_insert(now);
        *next_chat = (*next_chat).max(until);
    }
}

pub fn outbound_interval_for_chat(chat_id: i64) -> Duration {
    if chat_id < 0 {
        Duration::from_millis(TELEGRAM_GROUP_OUTBOUND_INTERVAL_MS)
    } else {
        Duration::from_millis(TELEGRAM_PRIVATE_OUTBOUND_INTERVAL_MS)
    }
}

fn telegram_retry_after(error: &anyhow::Error) -> Option<u64> {
    error
        .downcast_ref::<TelegramError>()
        .and_then(|telegram| telegram.retry_after)
}

#[derive(Debug)]
pub struct TelegramError {
    pub status: StatusCode,
    pub description: String,
    pub retry_after: Option<u64>,
}

impl fmt::Display for TelegramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "telegram api error {}: {}",
            self.status, self.description
        )
    }
}

impl Error for TelegramError {}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
    parameters: Option<ResponseParameters>,
}

#[derive(Debug, Deserialize)]
struct ResponseParameters {
    retry_after: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub message_thread_id: Option<i64>,
    pub from: Option<User>,
    pub chat: Chat,
    pub text: Option<String>,
    pub caption: Option<String>,
    #[serde(default)]
    pub photo: Vec<PhotoSize>,
    pub document: Option<Document>,
    pub audio: Option<Audio>,
    pub voice: Option<Voice>,
    pub video: Option<Video>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    pub message: Option<Message>,
    pub data: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: i64,
    pub is_bot: bool,
    #[allow(dead_code)]
    pub first_name: String,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub is_forum: Option<bool>,
    pub username: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub width: i64,
    pub height: i64,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Document {
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Audio {
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Voice {
    pub file_id: String,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Video {
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct File {
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ForumTopic {
    pub message_thread_id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BotCommand {
    pub command: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendMessage {
    pub chat_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_thread_id: Option<i64>,
    pub text: String,
    pub parse_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_preview_options: Option<LinkPreviewOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<InlineKeyboardMarkup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditMessageText {
    pub chat_id: i64,
    pub message_id: i64,
    pub text: String,
    pub parse_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_preview_options: Option<LinkPreviewOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<InlineKeyboardMarkup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendMessageDraft {
    pub chat_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_thread_id: Option<i64>,
    pub draft_id: i64,
    pub text: String,
    pub parse_mode: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InlineKeyboardButton {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkPreviewOptions {
    pub is_disabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatAction {
    Typing,
    UploadPhoto,
    UploadDocument,
    UploadVideo,
    UploadAudio,
}

impl ChatAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Typing => "typing",
            Self::UploadPhoto => "upload_photo",
            Self::UploadDocument => "upload_document",
            Self::UploadVideo => "upload_video",
            Self::UploadAudio => "upload_audio",
        }
    }
}

impl SendMessage {
    pub fn html(chat_id: i64, thread_id: Option<i64>, text: String) -> Self {
        Self {
            chat_id,
            message_thread_id: thread_id,
            text,
            parse_mode: "HTML".to_string(),
            link_preview_options: Some(LinkPreviewOptions { is_disabled: true }),
            reply_markup: None,
        }
    }
}

impl EditMessageText {
    pub fn html(chat_id: i64, message_id: i64, text: String) -> Self {
        Self {
            chat_id,
            message_id,
            text,
            parse_mode: "HTML".to_string(),
            link_preview_options: Some(LinkPreviewOptions { is_disabled: true }),
            reply_markup: None,
        }
    }
}

impl SendMessageDraft {
    pub fn html(chat_id: i64, thread_id: Option<i64>, draft_id: i64, text: String) -> Self {
        Self {
            chat_id,
            message_thread_id: thread_id,
            draft_id,
            text,
            parse_mode: "HTML".to_string(),
        }
    }
}

pub fn normalize_command(text: &str, bot_username: Option<&str>) -> Option<(String, String)> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut split = trimmed.splitn(2, char::is_whitespace);
    let raw_command = split.next()?.trim();
    let args = split.next().unwrap_or("").trim().to_string();
    let command_without_slash = raw_command.trim_start_matches('/');
    let (name, mention) = command_without_slash
        .split_once('@')
        .unwrap_or((command_without_slash, ""));
    if !mention.is_empty() {
        let expected = bot_username.unwrap_or_default();
        if !expected.is_empty() && !mention.eq_ignore_ascii_case(expected) {
            return None;
        }
    }
    Some((format!("/{}", name.to_lowercase()), args))
}

pub fn is_foreign_bot_command(text: &str, bot_username: Option<&str>) -> bool {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return false;
    }
    let raw_command = trimmed.split_whitespace().next().unwrap_or_default().trim();
    let command_without_slash = raw_command.trim_start_matches('/');
    let Some((_, mention)) = command_without_slash.split_once('@') else {
        return false;
    };
    let expected = bot_username.unwrap_or_default();
    !mention.is_empty() && !expected.is_empty() && !mention.eq_ignore_ascii_case(expected)
}

pub fn preferred_image_file_id(message: &Message) -> Option<&str> {
    if let Some(document) = &message.document {
        if document
            .mime_type
            .as_deref()
            .unwrap_or_default()
            .starts_with("image/")
        {
            return Some(document.file_id.as_str());
        }
    }

    message
        .photo
        .iter()
        .max_by_key(|size| size.file_size.unwrap_or(size.width * size.height))
        .map(|photo| photo.file_id.as_str())
}

#[cfg(test)]
mod rate_limit_tests {
    use super::*;

    #[test]
    fn outbound_limiter_spaces_group_messages() {
        let mut state = OutboundLimiterState::default();
        let now = Instant::now();

        let first = state.acquire(-100123, now);
        let second = state.acquire(-100123, now);

        assert_eq!(first, Ok(()));
        assert_eq!(
            second.unwrap_err().duration_since(now),
            Duration::from_millis(TELEGRAM_GROUP_OUTBOUND_INTERVAL_MS)
        );
    }

    #[test]
    fn outbound_limiter_spaces_private_messages() {
        let mut state = OutboundLimiterState::default();
        let now = Instant::now();

        let first = state.acquire(123, now);
        let second = state.acquire(123, now);

        assert_eq!(first, Ok(()));
        assert_eq!(
            second.unwrap_err().duration_since(now),
            Duration::from_millis(TELEGRAM_PRIVATE_OUTBOUND_INTERVAL_MS)
        );
    }

    #[test]
    fn outbound_limiter_applies_global_spacing_between_chats() {
        let mut state = OutboundLimiterState::default();
        let now = Instant::now();

        let first = state.acquire(1, now);
        let second = state.acquire(2, now);

        assert_eq!(first, Ok(()));
        assert_eq!(
            second.unwrap_err().duration_since(now),
            Duration::from_millis(TELEGRAM_GLOBAL_OUTBOUND_INTERVAL_MS)
        );
    }

    #[test]
    fn outbound_limiter_extends_chat_after_retry_after() {
        let mut state = OutboundLimiterState::default();
        let now = Instant::now();
        state.backoff(-100123, now, 7);

        let next = state.acquire(-100123, now);

        assert_eq!(
            next.unwrap_err().duration_since(now),
            Duration::from_secs(8)
        );
    }

    #[test]
    fn outbound_limiter_rechecks_backoff_after_waiting() {
        let mut state = OutboundLimiterState::default();
        let now = Instant::now();

        assert_eq!(state.acquire(-100123, now), Ok(()));
        let queued_at = state.acquire(-100123, now).unwrap_err();
        state.backoff(-100123, now, 7);

        assert_eq!(
            queued_at.duration_since(now),
            Duration::from_millis(TELEGRAM_GROUP_OUTBOUND_INTERVAL_MS)
        );
        assert_eq!(
            state
                .acquire(-100123, queued_at)
                .unwrap_err()
                .duration_since(now),
            Duration::from_secs(8)
        );
    }

    #[test]
    fn send_message_draft_payload_includes_draft_id() {
        let payload = SendMessageDraft::html(42, Some(7), 123, "partial".to_string());
        let value = serde_json::to_value(payload).unwrap();

        assert_eq!(value["chat_id"], 42);
        assert_eq!(value["message_thread_id"], 7);
        assert_eq!(value["draft_id"], 123);
        assert_eq!(value["text"], "partial");
        assert_eq!(value["parse_mode"], "HTML");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_foreign_bot_command_mentions() {
        assert!(is_foreign_bot_command(
            "/status@other_bot",
            Some("telecodex_bot")
        ));
        assert!(!is_foreign_bot_command(
            "/status@telecodex_bot",
            Some("telecodex_bot")
        ));
        assert!(!is_foreign_bot_command("/status", Some("telecodex_bot")));
    }
}
