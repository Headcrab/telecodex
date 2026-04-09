use super::presentation::{
    ForumEnvironmentBindingKey, environment_topic_name, session_environment_binding_key,
    session_matches_environment,
};
use super::support::{
    forum_sync_cooldown_active, forum_sync_cooldown_key, forum_sync_error_key,
    is_forum_topic_not_modified, is_invalid_forum_topic_error, normalize_forum_sync_issue,
    prefer_primary_environment_session, telegram_retry_after,
};
use super::*;

pub(super) fn environment_sync_thread_binding<'a>(
    session: &crate::models::SessionRecord,
    environment: &'a CodexEnvironmentSummary,
) -> Option<&'a str> {
    if session.codex_thread_id.is_some() || session.force_fresh_thread {
        return None;
    }
    environment.latest_thread_id.as_deref()
}

impl App {
    pub(super) async fn ensure_environment_topic(
        &self,
        chat: &crate::telegram::Chat,
        current_thread_id: Option<i64>,
        environment_selector: &str,
    ) -> Result<()> {
        if forum_sync_cooldown_active(&self.shared.store, chat.id)? {
            return Ok(());
        }
        let environments = list_environments_for_sources(
            &default_codex_home(),
            200,
            self.shared.config.codex.import_desktop_history,
            self.shared.config.codex.import_cli_history,
            &self.shared.config.codex.seed_workspaces,
        )?;
        let Some(environment) = environments
            .into_iter()
            .find(|entry| environment_selector_key(entry) == environment_selector)
        else {
            self.send_status(
                chat.id,
                current_thread_id,
                "Environment is no longer available for import.",
            )
            .await?;
            return Ok(());
        };
        let sessions = self
            .prune_missing_forum_sessions(chat, self.shared.store.list_chat_sessions(chat.id)?)
            .await?;
        let sessions = self
            .dedupe_forum_environment_sessions(chat.id, sessions)
            .await?;
        if let Some(existing) = sessions
            .iter()
            .find(|session| session_matches_environment(session, &environment))
            .cloned()
        {
            if self
                .sync_environment_topic_metadata(chat.id, &environment, &existing)
                .await?
            {
                return Ok(());
            }
        }

        let topic = self
            .shared
            .telegram
            .create_forum_topic(chat.id, &environment.name)
            .await
            .with_context(|| format!("failed to create forum topic for {}", environment.name))?;
        let session_key = SessionKey::new(chat.id, Some(topic.message_thread_id));
        self.ensure_session(session_key, self.shared.service_user_id)?;
        self.shared
            .store
            .set_session_cwd(session_key, &environment.cwd)?;
        self.shared
            .store
            .set_session_title(session_key, Some(&environment.name))?;
        if let Some(thread_id) = environment.latest_thread_id.as_deref() {
            self.shared
                .store
                .set_session_codex_thread(session_key, thread_id)?;
        }
        Ok(())
    }

    pub(super) async fn sync_environment_topic_metadata(
        &self,
        chat_id: i64,
        environment: &CodexEnvironmentSummary,
        session: &crate::models::SessionRecord,
    ) -> Result<bool> {
        if session.key.thread_id <= 0 {
            return Ok(false);
        }
        self.shared
            .store
            .set_session_cwd(session.key, &environment.cwd)?;
        self.shared
            .store
            .set_session_title(session.key, Some(&environment.name))?;
        if let Some(thread_id) = environment_sync_thread_binding(session, environment) {
            self.shared
                .store
                .set_session_codex_thread(session.key, thread_id)?;
        }
        if session.session_title.as_deref().map(str::trim) == Some(environment.name.as_str()) {
            return Ok(true);
        }
        if let Err(error) = self
            .shared
            .telegram
            .edit_forum_topic(chat_id, session.key.thread_id, &environment.name)
            .await
        {
            if is_forum_topic_not_modified(&error) {
                return Ok(true);
            }
            if self
                .handle_forum_topic_rate_limit(
                    chat_id,
                    &error,
                    &format!(
                        "renaming topic #{} to `{}`",
                        session.key.thread_id, environment.name
                    ),
                )
                .await?
            {
                return Ok(true);
            }
            if is_invalid_forum_topic_error(&error) {
                self.shared.store.delete_session(session.key)?;
                self.shared.store.audit(
                    None,
                    "forum_topic_deleted_invalid",
                    serde_json::json!({
                        "chat_id": chat_id,
                        "thread_id": session.key.thread_id,
                        "cwd": environment.cwd,
                        "name": environment.name,
                    }),
                )?;
                return Ok(false);
            }
            tracing::warn!(
                "failed to rename forum topic {} in {} to {}: {error:#}",
                session.key.thread_id,
                chat_id,
                environment.name
            );
        }
        Ok(true)
    }

    pub(super) async fn poll_background_maintenance(&self) -> Result<()> {
        self.sync_primary_forum_topics().await?;
        self.cleanup_stale_forum_topics().await?;
        Ok(())
    }

    pub(super) async fn run_background_maintenance_loop(&self) -> Result<()> {
        loop {
            if let Err(error) = self.poll_background_maintenance().await {
                tracing::error!("background maintenance failed: {error:#}");
            }
            sleep(Duration::from_secs(
                Self::BACKGROUND_MAINTENANCE_INTERVAL_SECONDS,
            ))
            .await;
        }
    }

    pub(super) async fn sync_primary_forum_topics(&self) -> Result<()> {
        self.sync_primary_forum_topics_with_limit(
            self.shared.config.telegram.forum_sync_topics_per_poll,
            self.shared.config.telegram.auto_create_topics,
        )
        .await
    }

    pub(super) async fn sync_primary_forum_topics_with_limit(
        &self,
        topic_limit: usize,
        create_missing_topics: bool,
    ) -> Result<()> {
        let Some(chat_id) = self.shared.config.telegram.primary_forum_chat_id else {
            return Ok(());
        };
        if forum_sync_cooldown_active(&self.shared.store, chat_id)? {
            return Ok(());
        }
        let environments = list_environments_for_sources(
            &default_codex_home(),
            200,
            self.shared.config.codex.import_desktop_history,
            self.shared.config.codex.import_cli_history,
            &self.shared.config.codex.seed_workspaces,
        )?;
        if environments.is_empty() {
            return Ok(());
        }
        let existing_sessions = self
            .prune_missing_forum_sessions(
                &crate::telegram::Chat {
                    id: chat_id,
                    kind: "supergroup".to_string(),
                    is_forum: Some(true),
                    username: None,
                    title: None,
                },
                self.shared.store.list_chat_sessions(chat_id)?,
            )
            .await?;
        let existing_sessions = self
            .dedupe_forum_environment_sessions(chat_id, existing_sessions)
            .await?;
        let mut created = 0usize;
        for environment in environments {
            if forum_sync_cooldown_active(&self.shared.store, chat_id)? {
                break;
            }
            if let Some(existing) = existing_sessions
                .iter()
                .find(|session| session_matches_environment(session, &environment))
            {
                if self
                    .sync_environment_topic_metadata(chat_id, &environment, existing)
                    .await?
                {
                    continue;
                }
            }
            if !create_missing_topics {
                continue;
            }
            if created >= topic_limit {
                break;
            }
            let topic_name = environment_topic_name(&environment);
            let topic = match self
                .shared
                .telegram
                .create_forum_topic(chat_id, &topic_name)
                .await
            {
                Ok(topic) => topic,
                Err(error) => {
                    if self
                        .handle_forum_topic_rate_limit(
                            chat_id,
                            &error,
                            &format!("creating topic `{topic_name}`"),
                        )
                        .await?
                    {
                        break;
                    }
                    self.report_forum_sync_issue(
                        chat_id,
                        &format!("failed to create topic `{topic_name}`: {error:#}"),
                    )
                    .await;
                    tracing::warn!(
                        "failed to create synced forum topic `{topic_name}` in {chat_id}: {error:#}"
                    );
                    continue;
                }
            };
            let session_key = SessionKey::new(chat_id, Some(topic.message_thread_id));
            self.ensure_session(session_key, self.shared.service_user_id)?;
            self.shared
                .store
                .set_session_cwd(session_key, &environment.cwd)?;
            self.shared
                .store
                .set_session_title(session_key, Some(&environment.name))?;
            if let Some(thread_id) = environment.latest_thread_id.as_deref() {
                self.shared
                    .store
                    .set_session_codex_thread(session_key, thread_id)?;
            }
            self.shared.store.audit(
                None,
                "forum_topic_synced",
                serde_json::json!({
                    "chat_id": chat_id,
                    "thread_id": topic.message_thread_id,
                    "topic_name": topic.name,
                    "cwd": environment.cwd,
                    "codex_thread_id": environment.latest_thread_id,
                }),
            )?;
            created += 1;
        }
        Ok(())
    }

    pub(super) async fn prune_missing_forum_sessions(
        &self,
        chat: &crate::telegram::Chat,
        sessions: Vec<crate::models::SessionRecord>,
    ) -> Result<Vec<crate::models::SessionRecord>> {
        if !chat.is_forum.unwrap_or(false) {
            return Ok(sessions);
        }
        let mut alive = Vec::with_capacity(sessions.len());
        for session in sessions {
            if session.key.thread_id == 0 {
                alive.push(session);
                continue;
            }
            match self
                .shared
                .telegram
                .send_chat_action(chat.id, Some(session.key.thread_id), ChatAction::Typing)
                .await
            {
                Ok(_) => alive.push(session),
                Err(error) if is_message_thread_not_found(&error) => {
                    self.shared.store.delete_session(session.key)?;
                    self.shared.store.audit(
                        None,
                        "forum_topic_pruned_missing",
                        serde_json::json!({
                            "chat_id": chat.id,
                            "thread_id": session.key.thread_id,
                            "cwd": session.cwd,
                            "codex_thread_id": session.codex_thread_id,
                        }),
                    )?;
                }
                Err(_) => alive.push(session),
            }
        }
        Ok(alive)
    }

    pub(super) async fn dedupe_forum_environment_sessions(
        &self,
        chat_id: i64,
        sessions: Vec<crate::models::SessionRecord>,
    ) -> Result<Vec<crate::models::SessionRecord>> {
        if forum_sync_cooldown_active(&self.shared.store, chat_id)? {
            return Ok(sessions);
        }
        let mut root_sessions = Vec::new();
        let mut grouped: HashMap<ForumEnvironmentBindingKey, Vec<crate::models::SessionRecord>> =
            HashMap::new();
        for session in sessions {
            if session.key.thread_id <= 0 {
                root_sessions.push(session);
                continue;
            }
            let Some(binding_key) = session_environment_binding_key(&session) else {
                root_sessions.push(session);
                continue;
            };
            grouped.entry(binding_key).or_default().push(session);
        }

        let mut unique_sessions = Vec::new();
        for (binding_key, mut duplicates) in grouped {
            if forum_sync_cooldown_active(&self.shared.store, chat_id)? {
                unique_sessions.append(&mut duplicates);
                continue;
            }
            duplicates.sort_by(|left, right| {
                prefer_primary_environment_session(right, &binding_key.cwd)
                    .cmp(&prefer_primary_environment_session(left, &binding_key.cwd))
                    .then_with(|| right.updated_at.cmp(&left.updated_at))
                    .then_with(|| right.id.cmp(&left.id))
            });
            let keep = duplicates.remove(0);
            unique_sessions.push(keep.clone());
            for duplicate in duplicates {
                if forum_sync_cooldown_active(&self.shared.store, chat_id)? {
                    unique_sessions.push(duplicate);
                    continue;
                }
                if self
                    .drop_duplicate_forum_environment_session(chat_id, &binding_key, &duplicate)
                    .await?
                {
                    continue;
                }
                unique_sessions.push(duplicate);
            }
        }

        unique_sessions.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        root_sessions.extend(unique_sessions);
        Ok(root_sessions)
    }

    pub(super) async fn drop_duplicate_forum_environment_session(
        &self,
        chat_id: i64,
        binding_key: &ForumEnvironmentBindingKey,
        session: &crate::models::SessionRecord,
    ) -> Result<bool> {
        let deleted = match self
            .shared
            .telegram
            .delete_forum_topic(chat_id, session.key.thread_id)
            .await
        {
            Ok(_) => true,
            Err(error) if is_invalid_forum_topic_error(&error) => true,
            Err(error)
                if self
                    .handle_forum_topic_rate_limit(
                        chat_id,
                        &error,
                        &format!("deleting duplicate topic #{}", session.key.thread_id),
                    )
                    .await? =>
            {
                false
            }
            Err(error) => match self
                .shared
                .telegram
                .close_forum_topic(chat_id, session.key.thread_id)
                .await
            {
                Ok(_) => true,
                Err(close_error) if is_invalid_forum_topic_error(&close_error) => true,
                Err(close_error)
                    if self
                        .handle_forum_topic_rate_limit(
                            chat_id,
                            &close_error,
                            &format!("closing duplicate topic #{}", session.key.thread_id),
                        )
                        .await? =>
                {
                    false
                }
                Err(close_error) => {
                    tracing::warn!(
                        "failed to remove duplicate forum topic {} in {} for {} [{}]: delete={error:#}; close={close_error:#}",
                        session.key.thread_id,
                        chat_id,
                        binding_key.cwd.display(),
                        binding_key.topic_title
                    );
                    false
                }
            },
        };
        if !deleted {
            return Ok(false);
        }
        self.shared.store.delete_session(session.key)?;
        self.shared.store.audit(
            None,
            "forum_topic_deduped",
            serde_json::json!({
                "chat_id": chat_id,
                "thread_id": session.key.thread_id,
                "cwd": session.cwd,
                "canonical_cwd": binding_key.cwd,
                "topic_title": binding_key.topic_title,
                "codex_thread_id": session.codex_thread_id,
            }),
        )?;
        Ok(true)
    }

    pub(super) async fn handle_forum_topic_rate_limit(
        &self,
        chat_id: i64,
        error: &anyhow::Error,
        action: &str,
    ) -> Result<bool> {
        let Some(retry_after) = telegram_retry_after(error) else {
            return Ok(false);
        };
        let until = Utc::now() + chrono::Duration::seconds(retry_after as i64 + 1);
        self.shared
            .store
            .save_bot_state(&forum_sync_cooldown_key(chat_id), &until.to_rfc3339())?;
        self.report_forum_sync_issue(
            chat_id,
            &format!("rate limited while {action}: retry after {retry_after}s"),
        )
        .await;
        tracing::warn!(
            "forum topic sync hit Telegram rate limit while {action}, backing off until {}",
            until.to_rfc3339()
        );
        Ok(true)
    }

    pub(super) async fn report_forum_sync_issue(&self, chat_id: i64, issue: &str) {
        let key = forum_sync_error_key(chat_id);
        let deduped_issue = normalize_forum_sync_issue(issue);
        match self.shared.store.bot_state_value(&key) {
            Ok(Some(existing)) if existing == deduped_issue => return,
            Ok(_) => {
                if let Err(error) = self.shared.store.save_bot_state(&key, &deduped_issue) {
                    tracing::warn!("failed to persist forum sync issue: {error:#}");
                }
            }
            Err(error) => {
                tracing::warn!("failed to load forum sync issue state: {error:#}");
            }
        }
        self.notify_primary_user(&format!("⚠️ Forum sync {chat_id}: {issue}"))
            .await;
    }

    pub(super) async fn cleanup_stale_forum_topics(&self) -> Result<()> {
        let Some(chat_id) = self.shared.config.telegram.primary_forum_chat_id else {
            return Ok(());
        };
        if forum_sync_cooldown_active(&self.shared.store, chat_id)? {
            return Ok(());
        }
        let Some(days) = self.shared.config.telegram.stale_topic_days else {
            return Ok(());
        };
        let action = self.shared.config.telegram.stale_topic_action;
        if action == crate::config::StaleTopicAction::None {
            return Ok(());
        }

        let cutoff = Utc::now() - chrono::Duration::days(days);
        for session in self.shared.store.list_chat_sessions(chat_id)? {
            if session.key.thread_id <= 0 || session.busy {
                continue;
            }
            let updated_at = DateTime::parse_from_rfc3339(&session.updated_at)
                .map(|value| value.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            if updated_at > cutoff {
                continue;
            }
            let marker_key = format!("forum_cleanup:{}:{}", chat_id, session.key.thread_id);
            let marker_value = format!("{}:{}", action.as_str(), session.updated_at);
            if self.shared.store.bot_state_value(&marker_key)?.as_deref() == Some(&marker_value) {
                continue;
            }
            let result = match action {
                crate::config::StaleTopicAction::Close => {
                    self.shared
                        .telegram
                        .close_forum_topic(chat_id, session.key.thread_id)
                        .await
                }
                crate::config::StaleTopicAction::Delete => {
                    self.shared
                        .telegram
                        .delete_forum_topic(chat_id, session.key.thread_id)
                        .await
                }
                crate::config::StaleTopicAction::None => Ok(true),
            };
            if let Err(error) = result {
                if let Some(retry_after) = telegram_retry_after(&error) {
                    let until = Utc::now() + chrono::Duration::seconds(retry_after as i64 + 1);
                    self.shared
                        .store
                        .save_bot_state(&forum_sync_cooldown_key(chat_id), &until.to_rfc3339())?;
                    tracing::warn!(
                        "forum topic cleanup hit Telegram rate limit, backing off until {}",
                        until.to_rfc3339()
                    );
                    break;
                }
                tracing::warn!(
                    "failed to {} stale forum topic {} in {}: {error:#}",
                    action.as_str(),
                    session.key.thread_id,
                    chat_id
                );
                continue;
            }
            if action == crate::config::StaleTopicAction::Delete {
                self.shared.store.delete_session(session.key)?;
            }
            self.shared
                .store
                .save_bot_state(&marker_key, &marker_value)?;
            self.shared.store.audit(
                None,
                "forum_topic_cleanup",
                serde_json::json!({
                    "chat_id": chat_id,
                    "thread_id": session.key.thread_id,
                    "session_title": session.session_title,
                    "updated_at": session.updated_at,
                    "action": action.as_str(),
                }),
            )?;
        }
        Ok(())
    }
}
