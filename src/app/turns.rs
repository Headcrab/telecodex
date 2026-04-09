use super::presentation::{approval_waiting_text, request_telegram_approval};
use super::support::{is_message_not_modified, session_title_is_present, telegram_retry_after};
use super::*;
use std::path::Path;
use uuid::Uuid;

pub(super) async fn process_turn(
    shared: Arc<AppShared>,
    cancel_slot: Arc<StdMutex<Option<CancellationToken>>>,
    queued: QueuedTurn,
) -> Result<()> {
    let session = shared.store.ensure_session(
        queued.request.session_key,
        queued.request.from_user_id,
        &shared.session_defaults,
    )?;
    let session = resolve_session_codex_binding_from_history(&shared, session)?;
    shared.store.set_session_busy(session.key, true)?;
    let turn_id = shared
        .store
        .record_turn_started(session.id, &queued.request)?;
    let turn_workspace = prepare_turn_workspace(&session, turn_id)?;

    let cancel = CancellationToken::new();
    *cancel_slot.lock().expect("cancel mutex poisoned") = Some(cancel.clone());
    let chat_action_task = spawn_chat_action_task(
        shared.telegram.clone(),
        session.key.chat_id,
        Some(session.key.thread_id).filter(|value| *value != 0),
        cancel.clone(),
    );
    let limits_inline = latest_limits_snapshot_from_shared(&shared)
        .await?
        .and_then(|snapshot| format_limits_inline(&snapshot));
    let thinking_text = "⏳";
    let placeholder_text = render_placeholder_html(thinking_text, limits_inline.as_deref());

    if shared.config.telegram.use_message_drafts && queued.chat_kind == "private" {
        if let Err(error) = shared
            .telegram
            .send_message_draft(
                session.key.chat_id,
                Some(session.key.thread_id).filter(|id| *id != 0),
                thinking_text,
            )
            .await
        {
            tracing::debug!(
                "sendMessageDraft failed, falling back to placeholder message: {error:#}"
            );
        }
    }

    let placeholder = shared
        .telegram
        .send_message(SendMessage::html(
            session.key.chat_id,
            Some(session.key.thread_id).filter(|value| *value != 0),
            placeholder_text,
        ))
        .await
        .context("failed to create placeholder message")?;
    let sink = Arc::new(Mutex::new(LiveTurnSink::new(
        shared.clone(),
        &session,
        limits_inline,
        TelegramMessageRef {
            chat_id: session.key.chat_id,
            message_id: placeholder.message_id,
        },
    )));
    let mut runtime_request = queued.request.clone();
    enrich_audio_transcripts(&shared, &mut runtime_request, &turn_workspace, &sink).await;
    let runtime_request = prepare_runtime_request(&session, &runtime_request, &turn_workspace);
    let runtime_request = enrich_runtime_request_with_codex_history(&session, runtime_request);

    let run_result = shared
        .codex
        .run_turn(&session, &runtime_request, cancel.clone(), {
            let sink = sink.clone();
            let shared = shared.clone();
            let approval_cancel = cancel.clone();
            let requester_user_id = queued.request.from_user_id;
            let session_key = session.key;
            move |event| {
                let sink = sink.clone();
                let shared = shared.clone();
                let cancel = approval_cancel.clone();
                async move {
                    match event {
                        CodexEvent::ApprovalRequest(request) => {
                            if let Err(error) = sink
                                .lock()
                                .await
                                .set_progress(approval_waiting_text(request.kind))
                                .await
                            {
                                tracing::debug!(
                                    "failed to update approval progress for {:?}: {error:#}",
                                    session_key
                                );
                            }
                            let decision = request_telegram_approval(
                                shared,
                                session_key.chat_id,
                                Some(session_key.thread_id).filter(|value| *value != 0),
                                requester_user_id,
                                request,
                                cancel,
                            )
                            .await?;
                            Ok(CodexEventOutcome::Approval(decision))
                        }
                        other => {
                            sink.lock().await.handle_event(other).await?;
                            Ok(CodexEventOutcome::None)
                        }
                    }
                }
            }
        })
        .await;

    *cancel_slot.lock().expect("cancel mutex poisoned") = None;
    cancel.cancel();
    let _ = chat_action_task.await;
    let final_result = async {
        match run_result {
            Ok(summary) => {
                shared.store.set_session_busy(session.key, false)?;
                let sink_for_success = sink.clone();
                let sink_for_failure = sink.clone();
                finalize_foreground_turn(
                        ForegroundTurnSuccess {
                            store: &shared.store,
                            session: &session,
                            turn_id,
                            review_mode: queued.request.review_mode.is_some(),
                            summary: &summary,
                        },
                        || async {
                            send_generated_artifacts(&shared, &session, &turn_workspace.out_dir)
                                .await
                        },
                        || async move { sink_for_success.lock().await.finish(None).await },
                        |message| async move {
                            sink_for_failure.lock().await.finish(Some(message)).await
                        },
                    )
                    .await
            }
            Err(error) => {
                let status = if error.to_string().contains("cancelled") {
                    "cancelled"
                } else {
                    "failed"
                };
                let recovery_note = if should_reset_session_after_error(&error) {
                    tracing::warn!(
                        "resetting stale Codex thread binding for {:?} after error: {error:#}",
                        session.key
                    );
                    match shared.store.clear_session_conversation(session.key) {
                        Ok(()) => Some(
                            "The saved Codex thread binding for this topic was reset. Retry the same request to start a fresh session."
                                .to_string(),
                        ),
                        Err(clear_error) => {
                            tracing::warn!(
                                "failed to clear stale session conversation for {:?}: {clear_error:#}",
                                session.key
                            );
                            shared.store.set_session_busy(session.key, false)?;
                            None
                        }
                    }
                } else {
                    shared.store.set_session_busy(session.key, false)?;
                    None
                };
                finish_failed_turn(&shared.store, turn_id, &sink, status, &error, recovery_note)
                    .await?;
                Err(error)
            }
        }
    }
    .await;

    finish_turn_cleanup(
        &queued.request.attachments,
        &turn_workspace.root,
        final_result,
    )
}

fn cleanup_paths(attachments: &[LocalAttachment], turn_root: &Path) {
    for path in attachments.iter().map(|attachment| &attachment.path) {
        if let Err(error) = fs::remove_file(path) {
            tracing::warn!(
                "failed to remove temp attachment {}: {error}",
                path.display()
            );
        }
    }
    if let Err(error) = fs::remove_dir_all(turn_root) {
        tracing::warn!(
            "failed to remove turn workspace {}: {error}",
            turn_root.display()
        );
    }
}

pub(super) fn finish_turn_cleanup<T>(
    attachments: &[LocalAttachment],
    turn_root: &Path,
    result: Result<T>,
) -> Result<T> {
    cleanup_paths(attachments, turn_root);
    result
}

pub(super) struct ForegroundTurnSuccess<'a> {
    pub(super) store: &'a Store,
    pub(super) session: &'a crate::models::SessionRecord,
    pub(super) turn_id: i64,
    pub(super) review_mode: bool,
    pub(super) summary: &'a crate::codex::RunSummary,
}

pub(super) async fn finalize_foreground_turn<
    SendArtifacts,
    SendArtifactsFut,
    FinishSuccess,
    FinishSuccessFut,
    FinishFailure,
    FinishFailureFut,
>(
    context: ForegroundTurnSuccess<'_>,
    send_artifacts: SendArtifacts,
    finish_success: FinishSuccess,
    finish_failure: FinishFailure,
) -> Result<()>
where
    SendArtifacts: FnOnce() -> SendArtifactsFut,
    SendArtifactsFut: std::future::Future<Output = Result<()>>,
    FinishSuccess: FnOnce() -> FinishSuccessFut,
    FinishSuccessFut: std::future::Future<Output = Result<()>>,
    FinishFailure: FnOnce(String) -> FinishFailureFut,
    FinishFailureFut: std::future::Future<Output = Result<()>>,
{
    let success_result = async {
        if !context.review_mode {
            if let Some(thread_id) = context.summary.codex_thread_id.as_deref() {
                context
                    .store
                    .set_session_codex_thread(context.session.key, thread_id)?;
            }
        }
        context
            .store
            .set_last_assistant_text(context.session.key, Some(&context.summary.assistant_text))?;
        send_artifacts()
            .await
            .context("failed to deliver generated artifacts")?;
        finish_success()
            .await
            .context("failed to finalize Telegram turn message")?;
        context.store.record_turn_finished(
            context.turn_id,
            "completed",
            Some(&context.summary.assistant_text),
        )?;
        Ok(())
    }
    .await;

    if let Err(error) = success_result {
        context
            .store
            .record_turn_finished(context.turn_id, "failed", None)?;
        if let Err(finish_error) = finish_failure(format!("Turn failed: {error:#}")).await {
            tracing::warn!("failed to report foreground turn failure: {finish_error:#}");
        }
        return Err(error);
    }

    Ok(())
}

async fn finish_failed_turn(
    store: &Store,
    turn_id: i64,
    sink: &Arc<Mutex<LiveTurnSink>>,
    status: &str,
    error: &anyhow::Error,
    recovery_note: Option<String>,
) -> Result<()> {
    store.record_turn_finished(turn_id, status, None)?;
    let mut message = format!("Turn {status}: {error:#}");
    if let Some(note) = recovery_note {
        message.push_str("\n\n");
        message.push_str(&note);
    }
    sink.lock().await.finish(Some(message)).await?;
    Ok(())
}

pub(super) fn should_reset_session_after_error(error: &anyhow::Error) -> bool {
    let pretty = format!("{error:#}").to_ascii_lowercase();
    let display = error.to_string().to_ascii_lowercase();
    let matches = |text: &str| {
        text.contains("no rollout found for thread id")
            || (text.contains("rollout")
                && text.contains("thread id")
                && text.contains("code -32600"))
    };
    matches(&pretty) || matches(&display)
}

struct LiveTurnSink {
    shared: Arc<AppShared>,
    session_key: SessionKey,
    messages: Vec<TelegramMessageRef>,
    limits_inline: Option<String>,
    pending_text: String,
    has_assistant_text: bool,
    last_flushed_text: String,
    last_flush_at: Instant,
    edit_backoff_until: Option<Instant>,
}

impl LiveTurnSink {
    fn new(
        shared: Arc<AppShared>,
        session: &crate::models::SessionRecord,
        limits_inline: Option<String>,
        placeholder: TelegramMessageRef,
    ) -> Self {
        Self {
            shared,
            session_key: session.key,
            messages: vec![placeholder],
            limits_inline,
            pending_text: "⏳".to_string(),
            has_assistant_text: false,
            last_flushed_text: String::new(),
            last_flush_at: Instant::now() - Duration::from_secs(60),
            edit_backoff_until: None,
        }
    }

    async fn handle_event(&mut self, event: CodexEvent) -> Result<()> {
        match event {
            CodexEvent::Progress(text) => {
                if !self.has_assistant_text {
                    self.pending_text = progress_status_text(&text);
                }
            }
            CodexEvent::AssistantText(text) => {
                self.pending_text = text;
                self.has_assistant_text = true;
            }
            CodexEvent::ThreadStarted(thread_id) => {
                tracing::debug!("codex thread started: {thread_id}");
            }
            CodexEvent::ApprovalRequest(request) => {
                if !self.has_assistant_text {
                    self.pending_text = approval_waiting_text(request.kind);
                }
            }
        }
        self.flush(false).await
    }

    async fn set_progress(&mut self, text: impl Into<String>) -> Result<()> {
        if !self.has_assistant_text {
            self.pending_text = progress_status_text(&text.into());
            self.flush(false).await?;
        }
        Ok(())
    }

    async fn finish(&mut self, final_error: Option<String>) -> Result<()> {
        if let Some(final_error) = final_error {
            self.pending_text = if self.pending_text.trim().is_empty() {
                final_error
            } else {
                format!("{}\n\n{}", self.pending_text, final_error)
            };
        }
        self.flush(true).await
    }

    async fn flush(&mut self, force: bool) -> Result<()> {
        if self
            .edit_backoff_until
            .is_some_and(|until| until <= Instant::now())
        {
            self.edit_backoff_until = None;
        }
        if self
            .edit_backoff_until
            .is_some_and(|until| until > Instant::now())
        {
            return Ok(());
        }
        let visible_text = self.visible_text(force);
        if !force
            && self.last_flushed_text == visible_text
            && self.last_flush_at.elapsed()
                < Duration::from_millis(self.shared.config.edit_debounce_ms)
        {
            return Ok(());
        }
        if !force
            && self.last_flush_at.elapsed()
                < Duration::from_millis(self.shared.config.edit_debounce_ms)
        {
            return Ok(());
        }

        let chunks = if force {
            split_text(&visible_text, self.shared.config.max_text_chunk)
        } else {
            vec![truncate_for_live_update(
                &visible_text,
                self.shared.config.max_text_chunk,
            )]
        };
        for (idx, chunk) in chunks.iter().enumerate() {
            let html = render_markdown_to_html(chunk);
            if let Some(existing) = self.messages.get(idx).cloned() {
                self.edit_message(existing, chunk, &html).await?;
            } else {
                let message = self
                    .shared
                    .telegram
                    .send_message(SendMessage::html(
                        self.session_key.chat_id,
                        Some(self.session_key.thread_id).filter(|value| *value != 0),
                        html.clone(),
                    ))
                    .await?;
                let reference = TelegramMessageRef {
                    chat_id: self.session_key.chat_id,
                    message_id: message.message_id,
                };
                self.messages.push(reference);
            }
        }

        self.last_flushed_text = visible_text;
        self.last_flush_at = Instant::now();
        Ok(())
    }

    fn visible_text(&self, force: bool) -> String {
        if force {
            return self.pending_text.clone();
        }
        match self.limits_inline.as_deref() {
            Some(limits_inline) if !limits_inline.is_empty() => {
                format!("{limits_inline}\n{}", self.pending_text)
            }
            _ => self.pending_text.clone(),
        }
    }

    async fn edit_message(
        &mut self,
        reference: TelegramMessageRef,
        raw_text: &str,
        html: &str,
    ) -> Result<()> {
        let result = self
            .shared
            .telegram
            .edit_message_text(EditMessageText::html(
                reference.chat_id,
                reference.message_id,
                html.to_string(),
            ))
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) => {
                if is_message_not_modified(&error) {
                    return Ok(());
                }
                if self.defer_after_retry_after(&error, "telegram edit") {
                    return Ok(());
                }
                let fallback = html_escape::encode_safe(raw_text).to_string();
                let fallback_result = self
                    .shared
                    .telegram
                    .edit_message_text(EditMessageText::html(
                        reference.chat_id,
                        reference.message_id,
                        fallback,
                    ))
                    .await;
                match fallback_result {
                    Ok(_) => Ok(()),
                    Err(fallback_error) if is_message_not_modified(&fallback_error) => Ok(()),
                    Err(fallback_error)
                        if self
                            .defer_after_retry_after(&fallback_error, "telegram edit fallback") =>
                    {
                        Ok(())
                    }
                    Err(fallback_error) => Err(fallback_error)
                        .with_context(|| format!("telegram edit fallback failed after: {error:#}")),
                }
            }
        }
    }

    fn defer_after_retry_after(&mut self, error: &anyhow::Error, label: &str) -> bool {
        let Some(retry_after) = telegram_retry_after(error) else {
            return false;
        };
        let until = Instant::now() + Duration::from_secs(retry_after.saturating_add(1));
        self.edit_backoff_until = Some(until);
        tracing::warn!("{label} hit Telegram rate limit, backing off for {retry_after}s");
        true
    }
}

pub(super) fn truncate_for_live_update(text: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    split_text(text, max_len)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn render_placeholder_html(status: &str, limits_banner: Option<&str>) -> String {
    let status = html_escape::encode_safe(status);
    match limits_banner {
        Some(limits_banner) if !limits_banner.is_empty() => {
            format!(
                "{}\n<i>{status}</i>",
                html_escape::encode_safe(limits_banner)
            )
        }
        _ => format!("<i>{status}</i>"),
    }
}

fn progress_status_text(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        "⏳".to_string()
    } else {
        format!("⏳ {text}")
    }
}

async fn enrich_audio_transcripts(
    shared: &Arc<AppShared>,
    request: &mut TurnRequest,
    workspace: &TurnWorkspace,
    sink: &Arc<Mutex<LiveTurnSink>>,
) {
    let Some(model_dir) = shared.handy_model_dir.clone() else {
        return;
    };

    let total = request
        .attachments
        .iter()
        .filter(|attachment| {
            matches!(
                attachment.kind,
                AttachmentKind::Audio | AttachmentKind::Voice
            )
        })
        .count();

    if total == 0 {
        return;
    }

    for (idx, attachment) in request
        .attachments
        .iter_mut()
        .filter(|attachment| {
            matches!(
                attachment.kind,
                AttachmentKind::Audio | AttachmentKind::Voice
            )
        })
        .enumerate()
    {
        let label = format!(
            "Transcribing audio {}/{} with Handy model...",
            idx + 1,
            total
        );
        if let Err(error) = sink.lock().await.set_progress(label).await {
            tracing::debug!("failed to update transcription progress: {error:#}");
        }

        match transcribe_audio_file(
            model_dir.clone(),
            attachment.path.clone(),
            workspace.root.clone(),
        )
        .await
        {
            Ok(transcript) => {
                attachment.transcript = Some(transcript);
            }
            Err(error) => {
                tracing::warn!(
                    "audio transcription failed for {}: {error:#}",
                    attachment.path.display()
                );
            }
        }
    }
}

fn has_preferred_audio_transcript(attachment: &LocalAttachment) -> bool {
    matches!(
        attachment.kind,
        AttachmentKind::Audio | AttachmentKind::Voice
    ) && attachment.transcript.is_some()
}

fn is_default_attachment_prompt(prompt: &str) -> bool {
    prompt.trim() == "Analyze the attached files."
}

fn prepare_turn_workspace(
    session: &crate::models::SessionRecord,
    turn_id: i64,
) -> Result<TurnWorkspace> {
    let root = session
        .cwd
        .join(".telecodex")
        .join("turns")
        .join(format!("{turn_id}-{}", Uuid::now_v7()));
    let out_dir = root.join("out");
    fs::create_dir_all(&out_dir)?;
    Ok(TurnWorkspace { root, out_dir })
}

pub(super) fn prepare_runtime_request(
    session: &crate::models::SessionRecord,
    request: &TurnRequest,
    workspace: &TurnWorkspace,
) -> TurnRequest {
    let mut user_prompt_sections = Vec::new();
    let mut instruction_sections = Vec::new();
    let non_transcript_attachment_lines = request
        .attachments
        .iter()
        .filter(|attachment| !has_preferred_audio_transcript(attachment))
        .map(|attachment| {
            let mime = attachment.mime_type.as_deref().unwrap_or("unknown");
            format!(
                "- {} -> {} ({mime}, {})",
                attachment.file_name,
                attachment.path.display(),
                attachment_kind_label(attachment.kind)
            )
        })
        .collect::<Vec<_>>();
    let transcript_texts = request
        .attachments
        .iter()
        .filter_map(|attachment| {
            attachment
                .transcript
                .as_ref()
                .map(|transcript| transcript.text.trim())
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();

    let omit_default_attachment_prompt = is_default_attachment_prompt(&request.prompt)
        && !transcript_texts.is_empty()
        && request
            .attachments
            .iter()
            .all(has_preferred_audio_transcript);

    if !request.prompt.trim().is_empty() && !omit_default_attachment_prompt {
        user_prompt_sections.push(request.prompt.trim().to_string());
    }

    for transcript_text in transcript_texts {
        user_prompt_sections.push(transcript_text.to_string());
    }

    if !non_transcript_attachment_lines.is_empty() {
        user_prompt_sections.push(format!(
            "Local files for this turn:\n{}\nUse these exact files as input if the request refers to an attachment.",
            non_transcript_attachment_lines.join("\n")
        ));
    }

    if !request.attachments.is_empty() && !non_transcript_attachment_lines.is_empty() {
        instruction_sections.push(format!(
            "Attached local files:\n{}\nRead and use them if relevant.",
            non_transcript_attachment_lines.join("\n")
        ));
    }

    instruction_sections.push(format!(
        "If you generate final deliverable files for the user, save only the final files in this directory:\n{}\nKeep intermediate scratch files outside this directory.",
        workspace.out_dir.display()
    ));

    let mut runtime_request = request.clone();
    runtime_request.prompt = user_prompt_sections.join("\n\n");
    runtime_request.runtime_instructions = Some(instruction_sections.join("\n\n"));
    if runtime_request.prompt.trim().is_empty() {
        runtime_request.prompt = "Follow the session instructions.".to_string();
    }
    if session.session_prompt.is_some() {
        tracing::debug!("session prompt is active for {:?}", session.key);
    }
    runtime_request
}

fn enrich_runtime_request_with_codex_history(
    session: &crate::models::SessionRecord,
    mut request: TurnRequest,
) -> TurnRequest {
    let Some(thread_id) = session.codex_thread_id.as_deref() else {
        return request;
    };
    let Ok(history) = read_thread_history(&default_codex_home(), thread_id, 8) else {
        return request;
    };
    if history.is_empty() {
        return request;
    }
    let context = format_codex_history_context(&history);
    if context.is_empty() {
        return request;
    }
    request.runtime_instructions = Some(match request.runtime_instructions.take() {
        Some(existing) if !existing.trim().is_empty() => format!("{existing}\n\n{context}"),
        _ => context,
    });
    request
}

pub(super) fn format_codex_history_context(entries: &[CodexHistoryEntry]) -> String {
    let mut lines = vec![
        "Recent conversation context from the selected Codex session:".to_string(),
        "Use this as prior chat history and continue from it instead of starting from scratch."
            .to_string(),
    ];
    for entry in entries {
        let label = if entry.role == "assistant" {
            "Assistant"
        } else {
            "User"
        };
        lines.push(format!(
            "{label}: {}",
            truncate_history_context(&entry.text).replace('\n', " ")
        ));
    }
    lines.join("\n")
}

fn truncate_history_context(text: &str) -> String {
    const LIMIT: usize = 900;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    text.chars().take(LIMIT).collect::<String>() + "..."
}

fn spawn_chat_action_task(
    telegram: TelegramClient,
    chat_id: i64,
    thread_id: Option<i64>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while !cancel.is_cancelled() {
            if let Err(error) = telegram
                .send_chat_action(chat_id, thread_id, ChatAction::Typing)
                .await
            {
                tracing::debug!("sendChatAction failed: {error:#}");
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = sleep(Duration::from_secs(4)) => {}
            }
        }
    })
}

async fn latest_limits_snapshot_from_shared(
    shared: &Arc<AppShared>,
) -> Result<Option<LimitsSnapshot>> {
    let mut cache = shared.limits_cache.lock().await;
    if let Some(cached) = cache.as_ref() {
        if cached.fetched_at.elapsed() < Duration::from_secs(60) {
            return Ok(Some(cached.snapshot.clone()));
        }
    }

    let snapshot = find_latest_limits_snapshot(&default_codex_home())?;
    if let Some(snapshot) = snapshot.clone() {
        *cache = Some(CachedLimitsSnapshot {
            fetched_at: Instant::now(),
            snapshot,
        });
    }
    Ok(snapshot)
}

pub(super) fn resolve_session_codex_binding_from_history(
    shared: &Arc<AppShared>,
    session: crate::models::SessionRecord,
) -> Result<crate::models::SessionRecord> {
    if session.codex_thread_id.is_some() || session.force_fresh_thread {
        return Ok(session);
    }
    let Some(summary) = latest_thread_for_cwd(&default_codex_home(), &session.cwd)? else {
        return Ok(session);
    };
    shared
        .store
        .set_session_codex_thread(session.key, &summary.id)?;
    if !session_title_is_present(&session) {
        shared.store.set_session_title(
            session.key,
            Some(summary.title.trim()).filter(|title| !title.is_empty()),
        )?;
    }
    shared
        .store
        .get_session(session.key)?
        .ok_or_else(|| anyhow!("failed to reload bound session"))
}

async fn send_generated_artifacts(
    shared: &Arc<AppShared>,
    session: &crate::models::SessionRecord,
    out_dir: &Path,
) -> Result<()> {
    if !out_dir.exists() {
        return Ok(());
    }

    let mut entries = fs::read_dir(out_dir)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            if metadata.is_file() { Some(path) } else { None }
        })
        .collect::<Vec<_>>();
    entries.sort();

    for path in entries.into_iter().take(10) {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let action = match extension.as_str() {
            "png" | "jpg" | "jpeg" | "webp" => ChatAction::UploadPhoto,
            "mp3" | "wav" | "m4a" | "ogg" => ChatAction::UploadAudio,
            "mp4" | "mov" | "webm" => ChatAction::UploadVideo,
            _ => ChatAction::UploadDocument,
        };
        let _ = shared
            .telegram
            .send_chat_action(
                session.key.chat_id,
                Some(session.key.thread_id).filter(|value| *value != 0),
                action,
            )
            .await;
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("artifact.bin")
            .to_string();
        let mime_type = mime_type_for_path(&path);
        match action {
            ChatAction::UploadPhoto => {
                shared
                    .telegram
                    .send_photo(
                        session.key.chat_id,
                        Some(session.key.thread_id).filter(|value| *value != 0),
                        &path,
                        &file_name,
                        mime_type.as_deref(),
                    )
                    .await?;
            }
            ChatAction::UploadAudio => {
                shared
                    .telegram
                    .send_audio(
                        session.key.chat_id,
                        Some(session.key.thread_id).filter(|value| *value != 0),
                        &path,
                        &file_name,
                        mime_type.as_deref(),
                    )
                    .await?;
            }
            ChatAction::UploadVideo => {
                shared
                    .telegram
                    .send_video(
                        session.key.chat_id,
                        Some(session.key.thread_id).filter(|value| *value != 0),
                        &path,
                        &file_name,
                        mime_type.as_deref(),
                    )
                    .await?;
            }
            _ => {
                shared
                    .telegram
                    .send_document(
                        session.key.chat_id,
                        Some(session.key.thread_id).filter(|value| *value != 0),
                        &path,
                        &file_name,
                        mime_type.as_deref(),
                    )
                    .await?;
            }
        }
    }

    Ok(())
}

fn attachment_kind_label(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "image",
        AttachmentKind::Text => "text",
        AttachmentKind::Audio => "audio",
        AttachmentKind::Voice => "voice",
        AttachmentKind::Video => "video",
        AttachmentKind::Document => "document",
    }
}

pub(super) fn classify_document_kind(
    mime_type: Option<&str>,
    file_name: Option<&str>,
) -> AttachmentKind {
    let mime_type = mime_type.unwrap_or_default().to_ascii_lowercase();
    if mime_type.starts_with("image/") {
        return AttachmentKind::Image;
    }
    if mime_type.starts_with("text/")
        || matches!(
            file_name
                .and_then(|name| Path::new(name).extension())
                .and_then(|value| value.to_str())
                .map(|value| value.to_ascii_lowercase())
                .as_deref(),
            Some("txt" | "md" | "json" | "yaml" | "yml" | "toml" | "csv" | "tsv" | "log")
        )
    {
        return AttachmentKind::Text;
    }
    AttachmentKind::Document
}

pub(super) fn sanitize_file_name(file_name: &str, fallback_extension: &str) -> String {
    let mut cleaned = file_name
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => ch,
            _ => '_',
        })
        .collect::<String>();
    if cleaned.is_empty() {
        cleaned = format!("file.{fallback_extension}");
    } else if Path::new(&cleaned).extension().is_none() && !fallback_extension.is_empty() {
        cleaned.push('.');
        cleaned.push_str(fallback_extension);
    }
    cleaned
}

fn mime_type_for_path(path: &Path) -> Option<String> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png".to_string()),
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        "webp" => Some("image/webp".to_string()),
        "gif" => Some("image/gif".to_string()),
        "mp3" => Some("audio/mpeg".to_string()),
        "wav" => Some("audio/wav".to_string()),
        "m4a" => Some("audio/mp4".to_string()),
        "ogg" => Some("audio/ogg".to_string()),
        "mp4" => Some("video/mp4".to_string()),
        "mov" => Some("video/quicktime".to_string()),
        "webm" => Some("video/webm".to_string()),
        "pdf" => Some("application/pdf".to_string()),
        "txt" => Some("text/plain".to_string()),
        "md" => Some("text/markdown".to_string()),
        "json" => Some("application/json".to_string()),
        _ => None,
    }
}
