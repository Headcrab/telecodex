//! Topic-icon picker state, keyboards and callbacks without Codex/model calls.

use super::*;
use crate::telegram::TopicIconSticker;

const PAGE_SIZE: usize = 24;
const COLUMNS: usize = 6;
const PICKER_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_PICKERS: usize = 64;
const PREFIX: &str = "topic_icon:";

#[derive(Clone)]
/// An expiring picker bound to one requester, topic and Telegram message.
pub(super) struct PendingIconPicker {
    requester: i64,
    key: SessionKey,
    message_id: i64,
    created_at: Instant,
    icons: Vec<TopicIconSticker>,
}

impl PendingIconPicker {
    /// Require the original user, chat, topic and message within the picker TTL.
    fn matches(&self, message: &Message, requester: i64) -> bool {
        self.requester == requester
            && self.key == SessionKey::new(message.chat.id, message.message_thread_id)
            && self.message_id == message.message_id
            && self.created_at.elapsed() < PICKER_TTL
    }
}

/// Encode a picker action with its token in an inline keyboard button.
fn button(text: &str, token: &str, action: &str) -> InlineKeyboardButton {
    InlineKeyboardButton {
        text: text.to_string(),
        callback_data: Some(format!("{PREFIX}{token}:{action}")),
        url: None,
    }
}

/// Render a catalog page and its controls, returning `None` for invalid pages.
fn picker_page(
    icons: &[TopicIconSticker],
    token: &str,
    page: usize,
) -> Option<(String, InlineKeyboardMarkup)> {
    let pages = icons.len().div_ceil(PAGE_SIZE);
    if page >= pages {
        return None;
    }
    let start = page * PAGE_SIZE;
    let mut rows = icons[start..icons.len().min(start + PAGE_SIZE)]
        .chunks(COLUMNS)
        .enumerate()
        .map(|(row_index, row)| {
            row.iter()
                .enumerate()
                .map(|(column, icon)| {
                    button(
                        icon.emoji.as_deref().unwrap_or("?"),
                        token,
                        &format!("s:{}", start + row_index * COLUMNS + column),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut navigation = Vec::new();
    if page > 0 {
        navigation.push(button("‹ Previous", token, &format!("p:{}", page - 1)));
    }
    if page + 1 < pages {
        navigation.push(button("Next ›", token, &format!("p:{}", page + 1)));
    }
    if !navigation.is_empty() {
        rows.push(navigation);
    }
    rows.push(vec![
        button("Default icon", token, "clear"),
        button("Cancel", token, "cancel"),
    ]);
    Some((
        format!(
            "Choose an icon for this topic.\nPage {} of {} · {} icons",
            page + 1,
            pages,
            icons.len()
        ),
        InlineKeyboardMarkup {
            inline_keyboard: rows,
        },
    ))
}

impl App {
    /// Fetch Telegram's catalog and register a picker after confirming its message.
    ///
    /// Root chats and the General topic receive a status message instead. Picker
    /// state stays in memory, so restarting the bridge invalidates old callbacks.
    pub(super) async fn show_topic_icon_picker(
        &self,
        message: &Message,
        requester: i64,
    ) -> Result<()> {
        let key = SessionKey::new(message.chat.id, message.message_thread_id);
        if key.thread_id <= 0 {
            return self
                .send_status(
                    message.chat.id,
                    message.message_thread_id,
                    "Use /icon inside the topic you want to customize.",
                )
                .await;
        }
        if key.thread_id == 1 {
            return self
                .send_status(
                    message.chat.id,
                    message.message_thread_id,
                    "Telegram does not allow changing the General topic icon.",
                )
                .await;
        }
        let icons = match self.shared.telegram.get_forum_topic_icon_stickers().await {
            Ok(icons) => icons,
            Err(_) => {
                return self
                    .send_status(
                        message.chat.id,
                        message.message_thread_id,
                        "Could not load Telegram's topic icons. Try /icon again later.",
                    )
                    .await;
            }
        };
        let icons = icons
            .into_iter()
            .filter(|icon| {
                icon.emoji.as_ref().is_some_and(|value| !value.is_empty())
                    && icon
                        .custom_emoji_id
                        .as_ref()
                        .is_some_and(|value| !value.is_empty())
            })
            .collect::<Vec<_>>();
        let token = Uuid::now_v7().simple().to_string();
        let Some((text, keyboard)) = picker_page(&icons, &token, 0) else {
            return self
                .send_status(
                    message.chat.id,
                    message.message_thread_id,
                    "Telegram returned no topic icons. Try /icon again later.",
                )
                .await;
        };
        let mut request = SendMessage::html(key.chat_id, Some(key.thread_id), text);
        request.reply_markup = Some(keyboard);
        let receipt = self.shared.telegram.send_message(request).await?;
        if receipt.chat.id != key.chat_id
            || receipt.message_thread_id != Some(key.thread_id)
            || receipt.from.as_ref().map(|user| user.id) != Some(self.shared.bot_id)
        {
            bail!("Telegram did not confirm the topic icon picker destination");
        }
        let mut pickers = self.shared.pending_icon_pickers.lock().await;
        pickers.retain(|_, picker| picker.created_at.elapsed() < PICKER_TTL);
        if pickers.len() >= MAX_PICKERS {
            if let Some(oldest) = pickers
                .iter()
                .min_by_key(|(_, picker)| picker.created_at)
                .map(|(token, _)| token.clone())
            {
                pickers.remove(&oldest);
            }
        }
        pickers.insert(
            token,
            PendingIconPicker {
                requester,
                key,
                message_id: receipt.message_id,
                created_at: Instant::now(),
                icons,
            },
        );
        Ok(())
    }

    /// Consume icon callbacks after validating ownership, binding and expiration.
    ///
    /// Return `false` for other callback prefixes. Close selections before editing
    /// the topic so retries cannot repeat effects; page changes keep the picker open.
    pub(super) async fn handle_icon_callback(
        &self,
        message: &Message,
        requester: i64,
        data: &str,
    ) -> Result<bool> {
        let Some(rest) = data.strip_prefix(PREFIX) else {
            return Ok(false);
        };
        let (token, action) = rest.split_once(':').unwrap_or((rest, ""));
        let picker = self
            .shared
            .pending_icon_pickers
            .lock()
            .await
            .get(token)
            .cloned();
        let Some(picker) = picker.filter(|picker| picker.matches(message, requester)) else {
            self.send_status(message.chat.id, message.message_thread_id, "This icon picker has expired or belongs to another user or topic. Open /icon again.").await?;
            return Ok(true);
        };
        if let Some(page) = action
            .strip_prefix("p:")
            .and_then(|value| value.parse::<usize>().ok())
        {
            if let Some((text, keyboard)) = picker_page(&picker.icons, token, page) {
                let mut request =
                    EditMessageText::html(picker.key.chat_id, picker.message_id, text);
                request.reply_markup = Some(keyboard);
                if let Err(error) = self.shared.telegram.edit_message_text(request).await {
                    if !is_message_not_modified(&error) {
                        self.send_status(
                            picker.key.chat_id,
                            Some(picker.key.thread_id),
                            "Could not display that page. Open /icon again.",
                        )
                        .await?;
                    }
                }
            }
            return Ok(true);
        }
        let selected = action
            .strip_prefix("s:")
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(|index| picker.icons.get(index));
        let (icon_id, label) = match action {
            "cancel" => (None, "Icon selection cancelled.".to_string()),
            "clear" => (Some(""), "Default topic icon restored.".to_string()),
            _ => match selected {
                Some(icon) => (
                    icon.custom_emoji_id.as_deref(),
                    format!(
                        "Topic icon updated: {}",
                        icon.emoji.as_deref().unwrap_or("")
                    ),
                ),
                None => return Ok(true),
            },
        };
        // Close before the mutation so duplicate callbacks never repeat an effect.
        self.shared.pending_icon_pickers.lock().await.remove(token);
        let text = if let Some(icon_id) = icon_id {
            match self.shared.telegram.edit_forum_topic_icon(picker.key.chat_id, picker.key.thread_id, icon_id).await {
                Ok(true) => label,
                Err(error) if is_forum_topic_not_modified(&error) => label,
                _ => "Telegram did not confirm the icon change. Check the topic before trying /icon again.".to_string(),
            }
        } else {
            label
        };
        let mut request = EditMessageText::html(
            picker.key.chat_id,
            picker.message_id,
            html_escape::encode_text(&text).to_string(),
        );
        request.reply_markup = Some(InlineKeyboardMarkup {
            inline_keyboard: vec![],
        });
        if self
            .shared
            .telegram
            .edit_message_text(request)
            .await
            .is_err()
        {
            self.send_status(picker.key.chat_id, Some(picker.key.thread_id), &text)
                .await?;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Expiration must invalidate an otherwise matching picker binding.
    fn an_expired_picker_cannot_authorize_an_icon_change() {
        let message: Message = serde_json::from_value(serde_json::json!({
            "message_id": 50, "message_thread_id": 7,
            "chat": { "id": 100, "type": "private" }
        }))
        .unwrap();
        let mut picker = PendingIconPicker {
            requester: 100,
            key: SessionKey::new(100, Some(7)),
            message_id: 50,
            created_at: Instant::now(),
            icons: vec![],
        };
        assert!(picker.matches(&message, 100));
        picker.created_at -= PICKER_TTL;
        assert!(!picker.matches(&message, 100));
    }
}
