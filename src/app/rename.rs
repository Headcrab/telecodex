use super::*;

fn validate_title(title: &str) -> Result<&str> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 128 || title.chars().any(char::is_control) {
        bail!("Use a name of 1–128 characters on one line.");
    }
    Ok(title)
}

pub(super) async fn sync_native_title(shared: &AppShared, key: SessionKey) -> Result<bool> {
    // Re-read after an await so a first-thread binding cannot overwrite a newer human rename.
    for _ in 0..2 {
        let Some(title) = shared.store.explicit_session_title(key)? else {
            return Ok(false);
        };
        let Some(thread_id) = shared
            .store
            .get_session(key)?
            .and_then(|s| s.codex_thread_id)
        else {
            return Ok(false);
        };
        shared.codex.set_thread_name(&thread_id, &title).await?;
        if shared.store.explicit_session_title(key)?.as_deref() == Some(&title)
            && shared
                .store
                .get_session(key)?
                .and_then(|s| s.codex_thread_id)
                .as_deref()
                == Some(&thread_id)
        {
            return Ok(true);
        }
    }
    bail!("A newer rename is pending; retry /rename with the current name.")
}

impl App {
    pub(super) async fn rename_session(
        &self,
        user: &crate::models::UserRecord,
        message: &Message,
        title: &str,
        edit_topic: bool,
    ) -> Result<()> {
        let title = match validate_title(title) {
            Ok(title) => title,
            Err(error) => {
                return self
                    .send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &error.to_string(),
                    )
                    .await;
            }
        };
        if is_primary_forum_dashboard(
            &self.shared.config,
            &message.chat,
            message.message_thread_id,
        ) {
            return self
                .send_status(
                    message.chat.id,
                    message.message_thread_id,
                    "Open a work topic, then use /rename <new name>.",
                )
                .await;
        }
        let key = SessionKey::new(message.chat.id, message.message_thread_id);
        self.ensure_session(key, user.tg_user_id)?;
        self.shared.store.set_explicit_session_title(key, title)?;
        let native = match sync_native_title(&self.shared, key).await {
            Ok(true) => "Codex renamed.",
            Ok(false) => "Codex will receive this name when the session is first started.",
            Err(_) => {
                "Codex rename was not confirmed. Retry /rename with this name to reconcile it."
            }
        };
        let topic = if edit_topic && key.thread_id > 0 {
            match self
                .shared
                .telegram
                .edit_forum_topic(key.chat_id, key.thread_id, title)
                .await
            {
                Ok(true) => "Telegram topic renamed.",
                Err(error) if is_forum_topic_not_modified(&error) => {
                    "Telegram topic already has this name."
                }
                _ => {
                    "Telegram topic rename was not confirmed. Retry /rename with this name to reconcile it."
                }
            }
        } else if key.thread_id > 0 {
            "Telegram topic renamed."
        } else {
            ""
        };
        self.send_status(
            key.chat_id,
            message.message_thread_id,
            &format!("Name: {title}\n{native}\n{topic}"),
        )
        .await
    }
}
