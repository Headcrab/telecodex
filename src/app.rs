use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    time::sleep,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod auth;
mod forum;
mod io;
mod presentation;
mod support;
mod turns;

use crate::{
    codex::{
        AvailableModel, CodexApprovalDecision, CodexApprovalKind, CodexEvent, CodexEventOutcome,
        CodexRunner,
    },
    codex_history::{
        CodexEnvironmentSummary, CodexHistoryEntry, CodexThreadSummary,
        environment_identity_for_cwd, environment_selector_key, find_thread_by_id,
        find_thread_by_prefix, latest_thread_for_cwd, list_environments_for_sources,
        list_threads_for_cwd, read_thread_history,
    },
    commands::{
        BridgeCommand, CommandHelp, ParsedInput, command_help, default_bot_commands, parse_command,
    },
    config::Config,
    limits::{
        LimitsSnapshot, default_codex_home, find_latest_limits_snapshot, format_limits_inline,
        format_limits_summary,
    },
    models::{
        AttachmentKind, LocalAttachment, SessionKey, TelegramMessageRef, TurnRequest, UserRole,
    },
    render::{render_markdown_to_html, split_text},
    store::{SessionDefaults, Store},
    telegram::{
        ChatAction, EditMessageText, InlineKeyboardButton, InlineKeyboardMarkup, Message,
        SendMessage, TelegramClient, TelegramError, is_foreign_bot_command, normalize_command,
        preferred_image_file_id,
    },
    transcribe::{detect_handy_parakeet_model_dir, transcribe_audio_file},
};

use self::{auth::*, presentation::*, support::*, turns::*};

#[derive(Clone)]
pub struct App {
    shared: Arc<AppShared>,
    workers: Arc<Mutex<HashMap<SessionKey, SessionWorkerHandle>>>,
}

struct AppShared {
    config: Config,
    store: Store,
    telegram: TelegramClient,
    codex: CodexRunner,
    bot_username: Option<String>,
    service_user_id: i64,
    handy_model_dir: Option<PathBuf>,
    session_defaults: SessionDefaults,
    limits_cache: Mutex<Option<CachedLimitsSnapshot>>,
    pending_approvals: Mutex<HashMap<String, PendingApproval>>,
    pending_codex_login: Mutex<Option<PendingCodexLogin>>,
    codex_login_backoff_until: Mutex<Option<Instant>>,
}

#[derive(Clone)]
struct SessionWorkerHandle {
    sender: mpsc::UnboundedSender<QueuedTurn>,
    cancel: Arc<StdMutex<Option<CancellationToken>>>,
}

#[derive(Clone)]
struct QueuedTurn {
    request: TurnRequest,
    chat_kind: String,
}

#[derive(Clone)]
struct CachedLimitsSnapshot {
    fetched_at: Instant,
    snapshot: LimitsSnapshot,
}

struct PendingApproval {
    requester_user_id: i64,
    responder: oneshot::Sender<CodexApprovalDecision>,
}

struct TurnWorkspace {
    root: PathBuf,
    out_dir: PathBuf,
}

impl App {
    const BACKGROUND_MAINTENANCE_INTERVAL_SECONDS: u64 = 60;

    pub async fn bootstrap(config: Config) -> Result<Self> {
        let token = config.telegram.resolve_token()?;
        let telegram = TelegramClient::new(token, config.telegram.api_base.clone());
        let me = telegram.get_me().await.context("telegram getMe failed")?;
        let handy_model_dir = detect_handy_parakeet_model_dir();
        let session_defaults = SessionDefaults::from(&config.codex);
        let store = Store::open(
            &config.db_path,
            &config.startup_admin_ids,
            &session_defaults,
        )?;
        let codex = CodexRunner::new(config.codex.binary.clone());
        let service_user_id = config.startup_admin_ids.first().copied().unwrap_or(0);

        Ok(Self {
            shared: Arc::new(AppShared {
                config,
                store,
                telegram,
                codex,
                bot_username: me.username,
                service_user_id,
                handy_model_dir,
                session_defaults,
                limits_cache: Mutex::new(None),
                pending_approvals: Mutex::new(HashMap::new()),
                pending_codex_login: Mutex::new(None),
                codex_login_backoff_until: Mutex::new(None),
            }),
            workers: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn run(self) -> Result<()> {
        self.shared
            .telegram
            .set_my_commands(&default_bot_commands())
            .await
            .context("failed to register bot commands")?;

        self.notify_primary_user(&format!("🟢 Telecodex {} started", app_version_label()))
            .await;

        let maintenance_app = self.clone();
        tokio::spawn(async move {
            if let Err(error) = maintenance_app.run_background_maintenance_loop().await {
                tracing::error!("background maintenance loop failed: {error:#}");
            }
        });

        let mut offset = self.shared.store.last_update_id()?.map(|value| value + 1);
        tracing::info!("telecodex started {}", app_version_label());

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("shutdown signal received");
                    self.notify_primary_user(&format!("🔴 Telecodex {} stopped", app_version_label()))
                        .await;
                    return Ok(());
                }
                result = self
                    .shared
                    .telegram
                    .get_updates(offset, self.shared.config.poll_timeout_seconds) => {
                    match result {
                        Ok(updates) => {
                            for update in updates {
                                offset = Some(update.update_id + 1);
                                self.shared.store.save_last_update_id(update.update_id)?;
                                if let Err(error) = self.process_update(update).await {
                                    tracing::error!("update processing failed: {error:#}");
                                }
                            }
                        }
                        Err(error) => {
                            if telegram_status(&error) == Some(reqwest::StatusCode::CONFLICT) {
                                self.notify_primary_user(&format!("🔴 Telecodex {} stopped: getUpdates conflict", app_version_label()))
                                    .await;
                                return Err(anyhow!(
                                    "telegram getUpdates conflict: another bot instance is already running"
                                ));
                            }
                            if let Some(retry_after) = telegram_retry_after(&error) {
                                tracing::warn!("telegram asked to back off for {retry_after}s");
                                sleep(Duration::from_secs(retry_after)).await;
                            } else {
                                tracing::error!("getUpdates failed: {error:#}");
                                sleep(Duration::from_secs(3)).await;
                            }
                        }
                    }
                }
            }
        }
    }

    async fn notify_primary_user(&self, text: &str) {
        let Some(user_id) = self.shared.config.startup_admin_ids.first().copied() else {
            return;
        };
        if let Err(error) = self.send_status(user_id, None, text).await {
            tracing::warn!("failed to notify primary user: {error:#}");
        }
    }

    async fn process_update(&self, update: crate::telegram::Update) -> Result<()> {
        if let Some(callback) = update.callback_query {
            self.process_callback_query(callback).await?;
            return Ok(());
        }
        let Some(message) = update.message else {
            return Ok(());
        };
        let Some(from) = &message.from else {
            return Ok(());
        };
        if from.is_bot {
            return Ok(());
        }

        let user = self.shared.store.get_user(from.id)?;
        let Some(user) = user.filter(|user| user.allowed) else {
            self.shared.store.audit(
                Some(from.id),
                "access_denied",
                serde_json::json!({
                    "chat_id": message.chat.id,
                    "thread_id": message.message_thread_id,
                }),
            )?;
            return Ok(());
        };

        let text = message
            .text
            .as_deref()
            .or(message.caption.as_deref())
            .unwrap_or("")
            .trim();
        if is_foreign_bot_command(text, self.shared.bot_username.as_deref()) {
            return Ok(());
        }
        if self.dispatch_command_text(&user, &message, text).await? {
            return Ok(());
        }
        let session_key = SessionKey::new(message.chat.id, message.message_thread_id);

        if is_primary_forum_dashboard(
            &self.shared.config,
            &message.chat,
            message.message_thread_id,
        ) {
            self.send_status(
                message.chat.id,
                message.message_thread_id,
                "This is the environments dashboard. Use `/environments` to import environments or `/sessions` to list topic sessions.",
            )
            .await?;
            return Ok(());
        }

        if !self
            .ensure_codex_authenticated(message.chat.id, message.message_thread_id)
            .await?
        {
            return Ok(());
        }

        let session = self.ensure_session(session_key, from.id)?;
        let session = self.resolve_session_codex_binding(session)?;
        let session = self.maybe_assign_session_title_from_text(session, text)?;
        self.announce_session_if_switched(from.id, &message.chat, session.key, &session)
            .await?;
        let attachments = self.download_attachments(&message, &session).await?;
        if text.is_empty() && attachments.is_empty() {
            return Ok(());
        }

        let prompt = if !text.is_empty() {
            text.to_string()
        } else {
            "Analyze the attached files.".to_string()
        };
        let request = TurnRequest {
            session_key,
            from_user_id: from.id,
            prompt,
            runtime_instructions: None,
            attachments,
            review_mode: None,
            override_search_mode: auto_search_mode_for_prompt(text),
        };
        self.enqueue_turn(request, &message.chat.kind).await?;
        Ok(())
    }

    async fn process_callback_query(&self, callback: crate::telegram::CallbackQuery) -> Result<()> {
        let Some(message) = callback.message else {
            return Ok(());
        };
        let user = self.shared.store.get_user(callback.from.id)?;
        let Some(user) = user.filter(|user| user.allowed) else {
            return Ok(());
        };
        self.shared
            .telegram
            .answer_callback_query(&callback.id)
            .await
            .ok();
        let Some(data) = callback.data else {
            return Ok(());
        };
        if let Some((token, decision)) = parse_approval_callback_data(&data) {
            let pending = {
                let mut approvals = self.shared.pending_approvals.lock().await;
                match approvals.remove(&token) {
                    Some(pending)
                        if pending.requester_user_id == callback.from.id
                            || user.role == UserRole::Admin =>
                    {
                        Some(pending)
                    }
                    Some(pending) => {
                        approvals.insert(token.clone(), pending);
                        None
                    }
                    None => None,
                }
            };
            match pending {
                Some(pending) => {
                    let _ = pending.responder.send(decision);
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("Approval decision: {}", approval_decision_status(decision)),
                    )
                    .await?;
                }
                None if user.role != UserRole::Admin => {
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        "This approval request belongs to another user or is already closed.",
                    )
                    .await?;
                }
                None => {
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        "Approval request is already closed.",
                    )
                    .await?;
                }
            }
            return Ok(());
        }
        if let Some(environment_thread_id) = data.strip_prefix("env:") {
            self.ensure_environment_topic(
                &message.chat,
                message.message_thread_id,
                environment_thread_id,
            )
            .await?;
            return Ok(());
        }
        if let Some(thread_id) = data.strip_prefix("ses:") {
            let thread_id = thread_id.parse::<i64>()?;
            self.switch_chat_session(&user, &message.chat, message.message_thread_id, thread_id)
                .await?;
            return Ok(());
        }
        if let Some(command_text) = data.strip_prefix("cmd:") {
            let _ = self
                .dispatch_command_text(&user, &message, command_text)
                .await?;
        }
        Ok(())
    }

    async fn switch_chat_session(
        &self,
        user: &crate::models::UserRecord,
        chat: &crate::telegram::Chat,
        current_thread_id: Option<i64>,
        target_thread_id: i64,
    ) -> Result<()> {
        let current_key = SessionKey::new(chat.id, current_thread_id);
        let target_key = SessionKey::new(chat.id, Some(target_thread_id));
        let current = self.ensure_session(current_key, user.tg_user_id)?;
        let Some(target) = self.shared.store.get_session(target_key)? else {
            self.send_status(
                chat.id,
                current_thread_id,
                &format!("Session topic `{target_thread_id}` not found in this chat."),
            )
            .await?;
            return Ok(());
        };
        self.shared
            .store
            .apply_session_template(current.key, &target)?;
        let current = self
            .shared
            .store
            .get_session(current.key)?
            .ok_or_else(|| anyhow!("failed to reload switched session"))?;
        self.announce_session_if_switched(user.tg_user_id, chat, current.key, &current)
            .await?;
        self.send_status(
            chat.id,
            current_thread_id,
            &format!(
                "Switched session to **{}**.",
                escape_markdown_label(&current_session_label(&current, chat))
            ),
        )
        .await?;
        Ok(())
    }

    async fn dispatch_command_text(
        &self,
        user: &crate::models::UserRecord,
        message: &Message,
        text: &str,
    ) -> Result<bool> {
        let session_key = SessionKey::new(message.chat.id, message.message_thread_id);
        let Some((command, args)) = normalize_command(text, self.shared.bot_username.as_deref())
        else {
            return Ok(false);
        };
        let parsed = match parse_command(&command, &args, text) {
            Ok(parsed) => parsed,
            Err(error) => {
                let help = command_help(&command, &args).unwrap_or(CommandHelp {
                    text: format!("Command error: {error}"),
                    quick_commands: Vec::new(),
                });
                self.send_command_help(message.chat.id, message.message_thread_id, &help)
                    .await?;
                return Ok(true);
            }
        };
        if parsed_input_requires_codex_auth(&parsed)
            && !self
                .ensure_codex_authenticated(message.chat.id, message.message_thread_id)
                .await?
        {
            return Ok(true);
        }
        if command_uses_session_context(&parsed) {
            let session = self.ensure_session(session_key, user.tg_user_id)?;
            let session = self.resolve_session_codex_binding(session)?;
            self.announce_session_if_switched(
                user.tg_user_id,
                &message.chat,
                session_key,
                &session,
            )
            .await?;
        }
        self.handle_command(user, message, session_key, parsed)
            .await?;
        Ok(true)
    }

    async fn handle_command(
        &self,
        user: &crate::models::UserRecord,
        message: &Message,
        session_key: SessionKey,
        parsed: ParsedInput,
    ) -> Result<()> {
        match parsed {
            ParsedInput::Forward(text) => {
                let session = self.ensure_session(session_key, user.tg_user_id)?;
                let request = TurnRequest {
                    session_key,
                    from_user_id: user.tg_user_id,
                    prompt: text,
                    runtime_instructions: None,
                    attachments: self.download_attachments(message, &session).await?,
                    review_mode: None,
                    override_search_mode: auto_search_mode_for_prompt(
                        message.text.as_deref().unwrap_or(""),
                    ),
                };
                self.enqueue_turn(request, &message.chat.kind).await?;
            }
            ParsedInput::Bridge(command) => match command {
                BridgeCommand::Login => {
                    self.handle_login_command(message).await?;
                }
                BridgeCommand::Logout => {
                    self.handle_logout_command(message).await?;
                }
                BridgeCommand::New { title } => {
                    self.handle_new_session(user, message, title).await?;
                }
                BridgeCommand::Topic { title } => {
                    self.handle_new_topic(user, message, title).await?;
                }
                BridgeCommand::Use { thread_id_prefix } => {
                    let session = self.ensure_session(session_key, user.tg_user_id)?;
                    let summary = if thread_id_prefix.eq_ignore_ascii_case("latest") {
                        latest_thread_for_cwd(&default_codex_home(), &session.cwd)?
                    } else {
                        find_thread_by_prefix(
                            &default_codex_home(),
                            &session.cwd,
                            &thread_id_prefix,
                        )?
                    };
                    let Some(summary) = summary else {
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            &format!(
                                "Codex session `{thread_id_prefix}` not found for `{}`.",
                                session.cwd.display()
                            ),
                        )
                        .await?;
                        return Ok(());
                    };
                    let session = self.bind_session_to_codex_summary(&session, &summary)?;
                    self.announce_session_if_switched(
                        user.tg_user_id,
                        &message.chat,
                        session.key,
                        &session,
                    )
                    .await?;
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!(
                            "Switched to Codex session `{}`.\n`{}`",
                            short_codex_thread_id(&summary.id),
                            summary.title
                        ),
                    )
                    .await?;
                    let history = read_thread_history(&default_codex_home(), &summary.id, 6)?;
                    if !history.is_empty() {
                        self.send_html_status(
                            message.chat.id,
                            message.message_thread_id,
                            &format_codex_history_preview_html(&history),
                            Some(&format_codex_history_preview_plain(&history)),
                        )
                        .await?;
                    }
                }
                BridgeCommand::Review(review) => {
                    let request = TurnRequest {
                        session_key,
                        from_user_id: user.tg_user_id,
                        prompt: review.prompt.clone().unwrap_or_default(),
                        runtime_instructions: None,
                        attachments: vec![],
                        review_mode: Some(review),
                        override_search_mode: None,
                    };
                    self.enqueue_turn(request, &message.chat.kind).await?;
                }
                BridgeCommand::Cd { path } => {
                    let path = validate_directory(&path)?;
                    self.ensure_session(session_key, user.tg_user_id)?;
                    self.shared.store.set_session_cwd(session_key, &path)?;
                    self.shared.store.audit(
                        Some(user.tg_user_id),
                        "session_cd",
                        serde_json::json!({ "chat_id": session_key.chat_id, "thread_id": session_key.thread_id, "cwd": path }),
                    )?;
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("Session cwd set to `{}`.", path.display()),
                    )
                    .await?;
                }
                BridgeCommand::Pwd => {
                    let session = self.ensure_session(session_key, user.tg_user_id)?;
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("`{}`", session.cwd.display()),
                    )
                    .await?;
                }
                BridgeCommand::Environments => {
                    let session = self.ensure_session(session_key, user.tg_user_id)?;
                    if is_primary_forum_dashboard(
                        &self.shared.config,
                        &message.chat,
                        message.message_thread_id,
                    ) {
                        self.sync_primary_forum_topics_with_limit(24, false).await?;
                        let environments = list_environments_for_sources(
                            &default_codex_home(),
                            200,
                            self.shared.config.codex.import_desktop_history,
                            self.shared.config.codex.import_cli_history,
                            &self.shared.config.codex.seed_workspaces,
                        )?;
                        let sessions = self
                            .prune_missing_forum_sessions(
                                &message.chat,
                                self.shared.store.list_chat_sessions(message.chat.id)?,
                            )
                            .await?;
                        let sessions = self
                            .dedupe_forum_environment_sessions(message.chat.id, sessions)
                            .await?;
                        if environments.is_empty() {
                            self.send_status(
                                message.chat.id,
                                message.message_thread_id,
                                "No Codex environments found for import.",
                            )
                            .await?;
                        } else {
                            let body = format_environment_dashboard(&environments);
                            send_markdown_message(
                                &self.shared.telegram,
                                message.chat.id,
                                message.message_thread_id,
                                &body,
                                environment_dashboard_keyboard(
                                    &message.chat,
                                    &session,
                                    &environments,
                                    &sessions,
                                ),
                            )
                            .await?;
                        }
                    } else {
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            "Environment import is only available in the primary forum dashboard.",
                        )
                        .await?;
                    }
                }
                BridgeCommand::Sessions => {
                    let session = self.ensure_session(session_key, user.tg_user_id)?;
                    if session.key.thread_id == 0 {
                        let sessions = self.shared.store.list_chat_sessions(message.chat.id)?;
                        if sessions.is_empty() {
                            self.send_status(
                                message.chat.id,
                                message.message_thread_id,
                                "No sessions in this chat yet.",
                            )
                            .await?;
                        } else {
                            let body =
                                format_sessions_overview(&sessions, session_key, &message.chat);
                            send_markdown_message(
                                &self.shared.telegram,
                                message.chat.id,
                                message.message_thread_id,
                                &body,
                                chat_sessions_keyboard(&session, &message.chat, &sessions),
                            )
                            .await?;
                        }
                    } else {
                        let session = self.resolve_session_codex_binding(session)?;
                        let sessions =
                            list_threads_for_cwd(&default_codex_home(), &session.cwd, 20)?;
                        let body = format_codex_sessions_overview(&sessions);
                        send_markdown_message(
                            &self.shared.telegram,
                            message.chat.id,
                            message.message_thread_id,
                            &body,
                            codex_sessions_keyboard(&session, &sessions),
                            )
                            .await?;
                    }
                }
                BridgeCommand::Status => {
                    if is_primary_forum_dashboard(
                        &self.shared.config,
                        &message.chat,
                        message.message_thread_id,
                    ) {
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            "This is the environments dashboard, not a work topic.\n\nOpen a topic from `/sessions` or `/environments`, then run `/status` there.",
                        )
                        .await?;
                    } else {
                        let session = self.ensure_session(session_key, user.tg_user_id)?;
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            &format_session_status(&session, &message.chat),
                        )
                        .await?;
                    }
                }
                BridgeCommand::Stop => {
                    if self.stop_session(session_key).await {
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            "Stop signal sent.",
                        )
                        .await?;
                    } else {
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            "No active turn in this session.",
                        )
                        .await?;
                    }
                }
                BridgeCommand::Allow { user_id } => {
                    ensure_admin(user)?;
                    let role = self
                        .shared
                        .store
                        .get_user(user_id)?
                        .map(|entry| entry.role)
                        .unwrap_or(UserRole::User);
                    self.shared.store.upsert_user(user_id, role, true)?;
                    self.shared.store.audit(
                        Some(user.tg_user_id),
                        "allow_user",
                        serde_json::json!({ "target_user_id": user_id }),
                    )?;
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("User `{user_id}` allowed."),
                    )
                    .await?;
                }
                BridgeCommand::Deny { user_id } => {
                    ensure_admin(user)?;
                    let role = self
                        .shared
                        .store
                        .get_user(user_id)?
                        .map(|entry| entry.role)
                        .unwrap_or(UserRole::User);
                    self.shared.store.upsert_user(user_id, role, false)?;
                    self.shared.store.audit(
                        Some(user.tg_user_id),
                        "deny_user",
                        serde_json::json!({ "target_user_id": user_id }),
                    )?;
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("User `{user_id}` denied."),
                    )
                    .await?;
                }
                BridgeCommand::Role { user_id, role } => {
                    ensure_admin(user)?;
                    let parsed_role = UserRole::try_from(role.as_str())?;
                    let allowed = self
                        .shared
                        .store
                        .get_user(user_id)?
                        .map(|entry| entry.allowed)
                        .unwrap_or(true);
                    self.shared
                        .store
                        .upsert_user(user_id, parsed_role, allowed)?;
                    self.shared.store.audit(
                        Some(user.tg_user_id),
                        "set_role",
                        serde_json::json!({ "target_user_id": user_id, "role": role }),
                    )?;
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("User `{user_id}` role set to `{role}`."),
                    )
                    .await?;
                }
                BridgeCommand::Model { model } => {
                    let session = self.ensure_session(session_key, user.tg_user_id)?;
                    if let Some(model) = model {
                        let next_model = if model == "-" || model.eq_ignore_ascii_case("default") {
                            self.shared.config.codex.default_model.clone()
                        } else {
                            Some(model)
                        };
                        self.shared
                            .store
                            .set_session_model(session_key, next_model.as_deref())?;
                        let label = next_model
                            .as_deref()
                            .or(self.shared.config.codex.default_model.as_deref())
                            .unwrap_or("Codex default");
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            &format!("Model set to `{label}`."),
                        )
                        .await?;
                    } else {
                        let label = session
                            .model
                            .as_deref()
                            .or(self.shared.config.codex.default_model.as_deref())
                            .unwrap_or("Codex default");
                        let auth_status = self.shared.codex.auth_status().await?;
                        let available_models = if auth_status.authenticated {
                            match self.shared.codex.read_models().await {
                                Ok(models) => models,
                                Err(error) => {
                                    tracing::warn!(
                                        "failed to read available Codex models: {error:#}"
                                    );
                                    Vec::new()
                                }
                            }
                        } else {
                            Vec::new()
                        };
                        let text = if auth_status.authenticated {
                            format_model_help_text(label, &available_models)
                        } else {
                            format!(
                                "{}\n\nLog in with `/login` to fetch the live model catalog from Codex.",
                                format_model_help_text(label, &available_models)
                            )
                        };
                        self.send_command_help(
                            message.chat.id,
                            message.message_thread_id,
                            &CommandHelp {
                                text,
                                quick_commands: model_quick_commands(
                                    &available_models,
                                    session.model.as_deref(),
                                    self.shared.config.codex.default_model.as_deref(),
                                ),
                            },
                        )
                        .await?;
                    }
                }
                BridgeCommand::Think { level } => {
                    let session = self.ensure_session(session_key, user.tg_user_id)?;
                    if let Some(level) = level {
                        let next_level = if is_clear_value(&level) {
                            None
                        } else {
                            let normalized = normalize_reasoning_effort(&level)?;
                            Some(normalized)
                        };
                        self.shared
                            .store
                            .set_session_reasoning_effort(session_key, next_level.as_deref())?;
                        let label = next_level.as_deref().unwrap_or("Codex default");
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            &format!("Reasoning effort set to `{label}`."),
                        )
                        .await?;
                    } else {
                        let label = session
                            .reasoning_effort
                            .as_deref()
                            .or(self.shared.config.codex.default_reasoning_effort.as_deref())
                            .unwrap_or("Codex default");
                        self.send_command_help(
                            message.chat.id,
                            message.message_thread_id,
                            &CommandHelp {
                                text: format!("Current reasoning effort: `{label}`\n\nChoose one:"),
                                quick_commands: vec![
                                    vec!["/think minimal".to_string(), "/think low".to_string()],
                                    vec!["/think medium".to_string(), "/think high".to_string()],
                                    vec!["/think default".to_string()],
                                ],
                            },
                        )
                        .await?;
                    }
                }
                BridgeCommand::Prompt { prompt } => {
                    let session = self.ensure_session(session_key, user.tg_user_id)?;
                    if let Some(prompt) = prompt {
                        let next_prompt = if is_clear_value(&prompt) {
                            None
                        } else {
                            Some(prompt)
                        };
                        self.shared
                            .store
                            .set_session_prompt(session_key, next_prompt.as_deref())?;
                        let body = match next_prompt {
                            Some(prompt) => {
                                format!("Session prompt set.\n\n```text\n{prompt}\n```")
                            }
                            None => "Session prompt cleared.".to_string(),
                        };
                        self.send_status(message.chat.id, message.message_thread_id, &body)
                            .await?;
                    } else if let Some(prompt) = session.session_prompt {
                        self.send_command_help(
                            message.chat.id,
                            message.message_thread_id,
                            &CommandHelp {
                                text: format!("Current session prompt:\n\n```text\n{prompt}\n```"),
                                quick_commands: vec![vec!["/prompt clear".to_string()]],
                            },
                        )
                        .await?;
                    } else {
                        self.send_command_help(
                            message.chat.id,
                            message.message_thread_id,
                            &CommandHelp {
                                text: "No session prompt is set.\n\nSet one with `/prompt You are concise`.".to_string(),
                                quick_commands: vec![vec!["/prompt You are concise".to_string()]],
                            },
                        )
                        .await?;
                    }
                }
                BridgeCommand::Approval { approval } => {
                    ensure_approval_policy(&approval)?;
                    self.ensure_session(session_key, user.tg_user_id)?;
                    self.shared
                        .store
                        .set_session_approval(session_key, &approval)?;
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("Approval policy set to `{approval}`."),
                    )
                    .await?;
                }
                BridgeCommand::Sandbox { sandbox } => {
                    ensure_sandbox_mode(&sandbox)?;
                    self.ensure_session(session_key, user.tg_user_id)?;
                    self.shared
                        .store
                        .set_session_sandbox(session_key, &sandbox)?;
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("Sandbox mode set to `{sandbox}`."),
                    )
                    .await?;
                }
                BridgeCommand::Search { mode } => {
                    self.ensure_session(session_key, user.tg_user_id)?;
                    self.shared
                        .store
                        .set_session_search_mode(session_key, mode)?;
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("Web search mode set to `{}`.", mode.as_codex_value()),
                    )
                    .await?;
                }
                BridgeCommand::AddDir { path } => {
                    let path = validate_directory(&path)?;
                    self.ensure_session(session_key, user.tg_user_id)?;
                    let add_dirs = self.shared.store.add_session_dir(session_key, &path)?;
                    let body = add_dirs
                        .iter()
                        .map(|entry| format!("- `{}`", entry.display()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("Writable dirs:\n{body}"),
                    )
                    .await?;
                }
                BridgeCommand::Limits => {
                    let auth_status = self.shared.codex.auth_status().await?;
                    if auth_status.authenticated {
                        if let Some(snapshot) = self.shared.codex.read_rate_limits().await? {
                            self.send_status(
                                message.chat.id,
                                message.message_thread_id,
                                &format_limits_summary(&snapshot),
                            )
                            .await?;
                            return Ok(());
                        }
                    }
                    if let Some(snapshot) = self.latest_limits_snapshot().await? {
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            &format_limits_summary(&snapshot),
                        )
                        .await?;
                    } else if auth_status.authenticated {
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            "No local Codex limits snapshot found yet.",
                        )
                        .await?;
                    } else {
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            "Codex is not logged in and there is no cached local limits snapshot yet.\n\nUse `/login` first.",
                        )
                        .await?;
                    }
                }
                BridgeCommand::Copy => {
                    if let Some(text) = self.shared.store.last_assistant_text(session_key)? {
                        self.send_status(message.chat.id, message.message_thread_id, &text)
                            .await?;
                    } else {
                        self.send_status(
                            message.chat.id,
                            message.message_thread_id,
                            "No assistant reply cached for this session.",
                        )
                        .await?;
                    }
                }
                BridgeCommand::Clear => {
                    self.ensure_session(session_key, user.tg_user_id)?;
                    self.shared.store.clear_session_conversation(session_key)?;
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        "This Telegram thread will start a fresh Codex session on the next turn.",
                    )
                    .await?;
                }
                BridgeCommand::RestartBot => {
                    ensure_admin(user)?;
                    spawn_restarted_process()?;
                    self.shared.store.audit(
                        Some(user.tg_user_id),
                        "restart_bot",
                        serde_json::json!({
                            "chat_id": message.chat.id,
                            "thread_id": message.message_thread_id,
                        }),
                    )?;
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("♻️ Restarting. {}", app_version_label()),
                    )
                    .await?;
                    self.notify_primary_user(&format!(
                        "🔴 Telecodex {} stopped: restart",
                        app_version_label()
                    ))
                    .await;
                    tokio::spawn(async move {
                        sleep(Duration::from_millis(750)).await;
                        std::process::exit(0);
                    });
                }
                BridgeCommand::Unsupported { command } => {
                    self.send_status(
                        message.chat.id,
                        message.message_thread_id,
                        &format!("{command} is not applicable in Telegram."),
                    )
                    .await?;
                }
            },
        }
        Ok(())
    }

    async fn handle_new_session(
        &self,
        user: &crate::models::UserRecord,
        message: &Message,
        title: Option<String>,
    ) -> Result<()> {
        if message.chat.is_forum.unwrap_or(false) && message.message_thread_id.is_none() {
            self.send_status(
                message.chat.id,
                message.message_thread_id,
                "Dashboard root is not a work topic. Use `/topic` to create a new topic or `/environments` to import one.",
            )
            .await?;
            return Ok(());
        }

        let session_key = SessionKey::new(message.chat.id, message.message_thread_id);
        let session = self.ensure_session(session_key, user.tg_user_id)?;
        self.shared.store.clear_session_conversation(session_key)?;
        if let Some(title) = title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            self.shared
                .store
                .set_session_title(session_key, Some(title))?;
        }
        self.send_status(
            message.chat.id,
            message.message_thread_id,
            &format!(
                "Fresh Codex session armed for this topic.\ncwd=`{}`",
                session.cwd.display()
            ),
        )
        .await?;
        Ok(())
    }

    async fn handle_new_topic(
        &self,
        user: &crate::models::UserRecord,
        message: &Message,
        title: Option<String>,
    ) -> Result<()> {
        let target_chat_id = self
            .shared
            .config
            .telegram
            .primary_forum_chat_id
            .unwrap_or(message.chat.id);
        if self.shared.config.telegram.primary_forum_chat_id.is_none()
            && !message.chat.is_forum.unwrap_or(false)
        {
            self.send_status(
                message.chat.id,
                message.message_thread_id,
                "This chat is not a forum. Set `telegram.primary_forum_chat_id` to create topics in a dedicated forum.",
            )
            .await?;
            return Ok(());
        }
        if message.chat.is_forum.unwrap_or(false) && message.message_thread_id.is_none() {
            self.send_status(
                message.chat.id,
                message.message_thread_id,
                "Run `/topic` inside a work topic so the current environment can be copied.",
            )
            .await?;
            return Ok(());
        }

        let current_key = SessionKey::new(message.chat.id, message.message_thread_id);
        let current = self.ensure_session(current_key, user.tg_user_id)?;
        let topic_name = title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Telecodex {}", Utc::now().format("%Y-%m-%d %H:%M:%S")));
        let topic = self
            .shared
            .telegram
            .create_forum_topic(target_chat_id, &topic_name)
            .await
            .context("createForumTopic failed")?;
        let session_key = SessionKey::new(target_chat_id, Some(topic.message_thread_id));
        self.ensure_session(session_key, user.tg_user_id)?;
        let mut template = current;
        template.session_title = Some(topic.name.clone());
        template.codex_thread_id = None;
        template.force_fresh_thread = true;
        self.shared
            .store
            .apply_session_template(session_key, &template)?;
        self.send_status(
            target_chat_id,
            Some(topic.message_thread_id),
            &format!(
                "New topic ready.\nthread_id=`{}`\ncwd=`{}`",
                topic.message_thread_id,
                template.cwd.display()
            ),
        )
        .await?;
        self.send_status(
            message.chat.id,
            message.message_thread_id,
            &format!(
                "Created topic `{}` in chat `{}` with thread_id `{}`.",
                topic.name, target_chat_id, topic.message_thread_id
            ),
        )
        .await?;
        Ok(())
    }

    fn ensure_session(
        &self,
        session_key: SessionKey,
        user_id: i64,
    ) -> Result<crate::models::SessionRecord> {
        self.shared
            .store
            .ensure_session(session_key, user_id, &self.shared.session_defaults)?;
        self.shared
            .store
            .get_session(session_key)?
            .ok_or_else(|| anyhow!("failed to reload ensured session"))
    }

    fn maybe_assign_session_title_from_text(
        &self,
        session: crate::models::SessionRecord,
        text: &str,
    ) -> Result<crate::models::SessionRecord> {
        if session_title_is_present(&session) {
            return Ok(session);
        }
        let Some(title) = derive_session_title_from_text(text) else {
            return Ok(session);
        };
        self.shared
            .store
            .set_session_title(session.key, Some(&title))?;
        self.shared
            .store
            .get_session(session.key)?
            .ok_or_else(|| anyhow!("failed to reload session title"))
    }

    fn resolve_session_codex_binding(
        &self,
        session: crate::models::SessionRecord,
    ) -> Result<crate::models::SessionRecord> {
        resolve_session_codex_binding_from_history(&self.shared, session)
    }

    fn bind_session_to_codex_summary(
        &self,
        session: &crate::models::SessionRecord,
        summary: &CodexThreadSummary,
    ) -> Result<crate::models::SessionRecord> {
        self.shared
            .store
            .set_session_codex_thread(session.key, &summary.id)?;
        if !session_title_is_present(session) {
            self.shared.store.set_session_title(
                session.key,
                Some(summary.title.trim()).filter(|title| !title.is_empty()),
            )?;
        }
        self.shared
            .store
            .get_session(session.key)?
            .ok_or_else(|| anyhow!("failed to reload bound session"))
    }

    async fn announce_session_if_switched(
        &self,
        user_id: i64,
        chat: &crate::telegram::Chat,
        session_key: SessionKey,
        session: &crate::models::SessionRecord,
    ) -> Result<()> {
        let state_key = active_session_state_key(user_id, chat.id);
        let current = active_session_identity(session_key, session);
        if self.shared.store.bot_state_value(&state_key)?.as_deref() == Some(current.as_str()) {
            return Ok(());
        }
        self.shared.store.save_bot_state(&state_key, &current)?;
        self.send_status(
            chat.id,
            Some(session_key.thread_id).filter(|value| *value != 0),
            &format!(
                "Current session: **{}**",
                escape_markdown_label(&current_session_label(session, chat))
            ),
        )
        .await
    }

    async fn enqueue_turn(&self, request: TurnRequest, chat_kind: &str) -> Result<()> {
        self.ensure_session(request.session_key, request.from_user_id)?;
        let handle = self.worker_for(request.session_key).await?;
        handle
            .sender
            .send(QueuedTurn {
                request,
                chat_kind: chat_kind.to_string(),
            })
            .map_err(|_| anyhow!("session worker dropped"))?;
        Ok(())
    }

    async fn worker_for(&self, key: SessionKey) -> Result<SessionWorkerHandle> {
        if let Some(existing) = self.workers.lock().await.get(&key).cloned() {
            return Ok(existing);
        }

        let (tx, mut rx) = mpsc::unbounded_channel::<QueuedTurn>();
        let cancel = Arc::new(StdMutex::new(None));
        let handle = SessionWorkerHandle {
            sender: tx.clone(),
            cancel: cancel.clone(),
        };
        self.workers.lock().await.insert(key, handle.clone());

        let shared = self.shared.clone();
        tokio::spawn(async move {
            while let Some(turn) = rx.recv().await {
                if let Err(error) = process_turn(shared.clone(), cancel.clone(), turn).await {
                    tracing::error!("turn failed for {:?}: {error:#}", key);
                }
            }
        });

        Ok(handle)
    }

    async fn stop_session(&self, key: SessionKey) -> bool {
        let handle = self.workers.lock().await.get(&key).cloned();
        let Some(handle) = handle else {
            return false;
        };
        if let Some(cancel) = handle.cancel.lock().expect("cancel mutex poisoned").clone() {
            cancel.cancel();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests;
