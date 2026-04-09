use std::{error::Error, fmt};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[derive(Clone)]
pub struct TelegramClient {
    http: reqwest::Client,
    token: String,
    api_base: String,
}

const TELEGRAM_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const TELEGRAM_GET_UPDATES_GRACE: Duration = Duration::from_secs(15);
const TELEGRAM_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const TELEGRAM_UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);

impl TelegramClient {
    pub fn new(token: String, api_base: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            token,
            api_base: api_base.trim_end_matches('/').to_string(),
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
        self.post("sendMessage", Some(&request)).await
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
        self.post("editMessageText", Some(&request)).await
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

        self.post("createForumTopic", Some(&Payload { chat_id, name }))
            .await
    }

    pub async fn close_forum_topic(&self, chat_id: i64, message_thread_id: i64) -> Result<bool> {
        #[derive(Serialize)]
        struct Payload {
            chat_id: i64,
            message_thread_id: i64,
        }

        self.post(
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

        self.post(
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

        self.post(
            "editForumTopic",
            Some(&Payload {
                chat_id,
                message_thread_id,
                name,
            }),
        )
        .await
    }

    pub async fn send_message_draft(
        &self,
        chat_id: i64,
        message_thread_id: Option<i64>,
        text: &str,
    ) -> Result<bool> {
        #[derive(Serialize)]
        struct Payload<'a> {
            chat_id: i64,
            message_thread_id: Option<i64>,
            text: &'a str,
        }

        self.post(
            "sendMessageDraft",
            Some(&Payload {
                chat_id,
                message_thread_id,
                text,
            }),
        )
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
        let url = format!("{}/bot{}/{}", self.api_base, self.token, method);
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("failed to read upload file {}", path.display()))?;
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
        .max_by_key(|size| size.file_size.unwrap_or((size.width * size.height) as i64))
        .map(|photo| photo.file_id.as_str())
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
