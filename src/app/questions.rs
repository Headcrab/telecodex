//! A pending question never blocks the native RPC loop or becomes a new turn.

use super::*;

pub(super) struct PendingDialogue {
    pub local_turn_id: i64,
    pub request_id: NativeRequestId,
    session: SessionKey,
    requester: i64,
    request: UserInputRequest,
    index: usize,
    message_id: i64,
    message_ids: Vec<i64>,
    awaiting_text: bool,
    answers: UserInputResponse,
    closed: CancellationToken,
    responder: oneshot::Sender<UserInputDecision>,
}

async fn show_question(
    shared: &AppShared,
    token: &str,
    pending: &mut PendingDialogue,
) -> Result<()> {
    let question = &pending.request.questions[pending.index];
    let mut text = format!(
        "Codex question {}/{}: {}\n\n{}",
        pending.index + 1,
        pending.request.questions.len(),
        question.header,
        question.question
    );
    let mut rows = Vec::new();
    if let Some(options) = &question.options {
        for (index, option) in options.iter().enumerate() {
            text.push_str(&format!("\n\n{} — {}", option.label, option.description));
            rows.push(vec![InlineKeyboardButton {
                text: option.label.chars().take(80).collect(),
                callback_data: Some(format!("q:{token}:{}:{index}", pending.index)),
                url: None,
            }]);
        }
    }
    if !pending.request.is_blocking {
        text.push_str("\n\nCodex can continue while this question is open.");
    }
    text.push_str("\n\nReply to this message to answer, or tap Write answer then send text. Other text keeps steering the turn.");
    rows.push(vec![
        InlineKeyboardButton {
            text: if question.is_other {
                "Other answer"
            } else {
                "Write answer"
            }
            .to_string(),
            callback_data: Some(format!("q:{token}:{}:text", pending.index)),
            url: None,
        },
        InlineKeyboardButton {
            text: "Cancel turn".to_string(),
            callback_data: Some(format!("q:{token}:{}:cancel", pending.index)),
            url: None,
        },
    ]);
    pending.message_ids.clear();
    // Bound UTF-16 length too: Telegram entities count astral characters twice.
    let chunks = split_text(&text, 1900);
    let count = chunks.len();
    for (index, chunk) in chunks.into_iter().enumerate() {
        let chunk = if index == 0 {
            chunk
        } else {
            format!(
                "Codex question {}/{} (continued)\n\n{chunk}",
                pending.index + 1,
                pending.request.questions.len()
            )
        };
        let mut outgoing = SendMessage::html(
            pending.session.chat_id,
            Some(pending.session.thread_id).filter(|id| *id != 0),
            html_escape::encode_safe(&chunk).to_string(),
        );
        if index + 1 == count {
            outgoing.reply_markup = Some(InlineKeyboardMarkup {
                inline_keyboard: rows.clone(),
            });
        }
        // Controls must never fall back to a different Telegram topic.
        let message = shared.telegram.send_message(outgoing).await?;
        if message.chat.id != pending.session.chat_id
            || message.message_thread_id.unwrap_or(0) != pending.session.thread_id
        {
            anyhow::bail!("native question delivery did not confirm its Telegram topic");
        }
        shared.store.record_question_message(pending.session, message.message_id, pending.local_turn_id,
            &serde_json::json!({"requestId":pending.request_id,"threadId":pending.request.thread_id,
                "turnId":pending.request.turn_id,"itemId":pending.request.item_id,"questionId":question.id}))?;
        pending.message_id = message.message_id;
        pending.message_ids.push(message.message_id);
    }
    Ok(())
}

pub(super) async fn begin_dialogue(
    shared: Arc<AppShared>,
    session: SessionKey,
    local_turn_id: i64,
    requester: i64,
    request_id: NativeRequestId,
    request: UserInputRequest,
    closed: CancellationToken,
) -> Result<oneshot::Receiver<UserInputDecision>> {
    request.validate()?;
    let token = Uuid::now_v7().simple().to_string();
    let (responder, receiver) = oneshot::channel();
    let mut pending = PendingDialogue {
        local_turn_id,
        request_id,
        session,
        requester,
        request,
        index: 0,
        message_id: 0,
        message_ids: Vec::new(),
        awaiting_text: false,
        answers: UserInputResponse::default(),
        responder,
        closed,
    };
    show_question(&shared, &token, &mut pending).await?;
    if !pending.closed.is_cancelled() && !pending.responder.is_closed() {
        shared.pending_questions.lock().await.insert(token, pending);
    }
    Ok(receiver)
}

async fn notice(shared: &AppShared, message: &Message, text: &str) -> Result<()> {
    shared
        .telegram
        .send_message(SendMessage::html(
            message.chat.id,
            message.message_thread_id,
            html_escape::encode_safe(text).to_string(),
        ))
        .await?;
    Ok(())
}

fn belongs(pending: &PendingDialogue, key: SessionKey, user: i64, message_id: i64) -> bool {
    pending.session == key
        && pending.requester == user
        && pending.message_id == message_id
        && !pending.responder.is_closed()
        && !pending.closed.is_cancelled()
}

async fn answer(
    shared: Arc<AppShared>,
    token: String,
    mut pending: PendingDialogue,
    value: String,
) -> Result<()> {
    if pending.closed.is_cancelled() || pending.responder.is_closed() {
        return Ok(());
    }
    if let Ok(update_id) = HUMAN_UPDATE_ID.try_with(|id| *id) {
        if let Err(error) = shared.store.begin_question_answer(
            update_id,
            pending.local_turn_id,
            &pending.request_id,
        ) {
            shared.pending_questions.lock().await.insert(token, pending);
            return Err(error);
        }
    }
    pending.answers.answers.insert(
        pending.request.questions[pending.index].id.clone(),
        QuestionAnswer {
            answers: vec![value],
        },
    );
    pending.index += 1;
    pending.awaiting_text = false;
    if pending.index == pending.request.questions.len() {
        // Send once. A closed native connection must never cause a new turn/replay.
        let _ = pending
            .responder
            .send(UserInputDecision::Answers(pending.answers));
    } else {
        let result = show_question(&shared, &token, &mut pending).await;
        if !pending.closed.is_cancelled() && !pending.responder.is_closed() {
            shared.pending_questions.lock().await.insert(token, pending);
        }
        result?;
    }
    Ok(())
}

pub(super) async fn handle_callback(
    shared: Arc<AppShared>,
    message: &Message,
    user: i64,
    data: &str,
) -> Result<bool> {
    if let Some(token) = data.strip_prefix("qopen:") {
        let key = SessionKey::new(message.chat.id, message.message_thread_id);
        let mut guard = shared.pending_questions.lock().await;
        let valid = guard.get(token).is_some_and(|p| {
            p.session == key
                && p.requester == user
                && !p.closed.is_cancelled()
                && !p.responder.is_closed()
        });
        if !valid {
            drop(guard);
            notice(
                &shared,
                message,
                "This question is no longer pending for you in this topic.",
            )
            .await?;
        } else {
            let mut pending = guard.remove(token).expect("checked pending question");
            drop(guard);
            let result = show_question(&shared, token, &mut pending).await;
            if !pending.closed.is_cancelled() && !pending.responder.is_closed() {
                shared
                    .pending_questions
                    .lock()
                    .await
                    .insert(token.to_string(), pending);
            }
            result?;
        }
        return Ok(true);
    }
    if !data.starts_with("q:") {
        return Ok(false);
    }
    let mut parts = data.split(':');
    let _ = parts.next();
    let token = parts.next().unwrap_or("");
    let index = parts.next().and_then(|s| s.parse::<usize>().ok());
    let action = parts.next().unwrap_or("");
    let key = SessionKey::new(message.chat.id, message.message_thread_id);
    let mut guard = shared.pending_questions.lock().await;
    let valid = guard.get(token).is_some_and(|p| {
        belongs(p, key, user, message.message_id)
            && Some(p.index) == index
            && parts.next().is_none()
    });
    if !valid {
        drop(guard);
        notice(
            &shared,
            message,
            "This question is closed or belongs to another user/topic. No answer was sent.",
        )
        .await?;
        return Ok(true);
    }
    if action == "text" {
        for pending in guard
            .values_mut()
            .filter(|p| p.session == key && p.requester == user)
        {
            pending.awaiting_text = false;
        }
        let mut pending = guard.remove(token).expect("checked pending question");
        pending.awaiting_text = true;
        drop(guard);
        let question = &pending.request.questions[pending.index];
        let prompt = format!(
            "Codex question {}/{}: {}\n\nSend your answer as text, or reply directly to the question. /stop cancels the turn.",
            pending.index + 1,
            pending.request.questions.len(),
            question.header
        );
        let sent = shared
            .telegram
            .send_force_reply(SendMessage::html(
                key.chat_id,
                Some(key.thread_id).filter(|id| *id != 0),
                html_escape::encode_safe(&prompt).to_string(),
            ))
            .await;
        let result = match sent {
            Ok(sent)
                if sent.chat.id == key.chat_id
                    && sent.message_thread_id.unwrap_or(0) == key.thread_id =>
            {
                let recorded = shared.store.record_question_message(key, sent.message_id, pending.local_turn_id,
                    &serde_json::json!({"requestId":pending.request_id,"threadId":pending.request.thread_id,
                        "turnId":pending.request.turn_id,"itemId":pending.request.item_id,"questionId":question.id}));
                pending.message_ids.push(sent.message_id);
                recorded
            }
            Ok(_) => Err(anyhow!(
                "free-text prompt delivery did not confirm its Telegram topic"
            )),
            Err(error) => Err(error),
        };
        if !pending.closed.is_cancelled() && !pending.responder.is_closed() {
            shared
                .pending_questions
                .lock()
                .await
                .insert(token.to_string(), pending);
        }
        result?;
    } else if action == "cancel" {
        let pending = guard.remove(token).expect("checked pending question");
        drop(guard);
        let _ = pending.responder.send(UserInputDecision::Cancel);
    } else {
        let value = guard.get(token).and_then(|p| {
            action
                .parse::<usize>()
                .ok()
                .and_then(|i| p.request.questions[p.index].options.as_ref()?.get(i))
                .map(|option| option.label.clone())
        });
        if let Some(value) = value {
            let pending = guard.remove(token).expect("checked pending question");
            drop(guard);
            answer(shared, token.to_string(), pending, value).await?;
        } else {
            drop(guard);
            notice(
                &shared,
                message,
                "Invalid answer control. No answer was sent.",
            )
            .await?;
        }
    }
    Ok(true)
}

pub(super) async fn handle_text_answer(
    shared: Arc<AppShared>,
    message: &Message,
    user: i64,
    text: &str,
) -> Result<bool> {
    if text.is_empty()
        || text.starts_with('/')
        || message.document.is_some()
        || !message.photo.is_empty()
        || message.audio.is_some()
        || message.voice.is_some()
        || message.video.is_some()
    {
        return Ok(false);
    }
    let key = SessionKey::new(message.chat.id, message.message_thread_id);
    let reply = message.reply_to_message.as_ref().map(|m| m.message_id);
    let mut guard = shared.pending_questions.lock().await;
    let token = guard
        .iter()
        .find(|(_, p)| {
            p.session == key
                && p.requester == user
                && !p.responder.is_closed()
                && !p.closed.is_cancelled()
                && reply.is_some_and(|id| p.message_ids.contains(&id))
        })
        .map(|(token, _)| token.clone());
    if let Some(token) = token {
        let pending = guard.remove(&token).expect("selected pending question");
        drop(guard);
        answer(shared, token, pending, text.to_string()).await?;
        return Ok(true);
    }
    drop(guard);
    if let Some(reply) = reply {
        // Telegram can display a question before its HTTP receipt reaches SQLite.
        // A crash in that gap must not turn a quoted answer into a fresh prompt.
        let unrecorded_question = message.reply_to_message.as_ref().is_some_and(|quoted| {
            quoted
                .from
                .as_ref()
                .is_some_and(|from| from.is_bot && from.id == shared.bot_id)
                && quoted
                    .text
                    .as_deref()
                    .is_some_and(|text| text.starts_with("Codex question "))
        });
        if shared.store.is_question_message(key, reply)? || unrecorded_question {
            notice(&shared,message,"This question is no longer waiting for your answer. No new turn was started; inspect /status before continuing.").await?;
            return Ok(true);
        }
    }
    // Arming explicitly selects the next text. Telegram topic clients can
    // attach a root-message reply even when the human did not quote a question.
    // Recognized expired questions above must never consume this capture.
    let mut guard = shared.pending_questions.lock().await;
    let token = guard
        .iter()
        .find(|(_, p)| {
            p.session == key
                && p.requester == user
                && p.awaiting_text
                && !p.closed.is_cancelled()
                && !p.responder.is_closed()
        })
        .map(|(token, _)| token.clone());
    if let Some(token) = token {
        let pending = guard.remove(&token).expect("armed question");
        drop(guard);
        answer(shared, token, pending, text.to_string()).await?;
        return Ok(true);
    }
    Ok(false)
}

pub(super) async fn show_pending(
    shared: Arc<AppShared>,
    message: &Message,
    user: i64,
) -> Result<()> {
    let key = SessionKey::new(message.chat.id, message.message_thread_id);
    let guard = shared.pending_questions.lock().await;
    let mut rows = guard
        .iter()
        .filter(|(_, p)| {
            p.session == key
                && p.requester == user
                && !p.closed.is_cancelled()
                && !p.responder.is_closed()
        })
        .map(|(token, p)| {
            (
                p.local_turn_id,
                p.request.item_id.clone(),
                vec![InlineKeyboardButton {
                    text: format!(
                        "{} ({}/{})",
                        p.request.questions[p.index].header,
                        p.index + 1,
                        p.request.questions.len()
                    ),
                    callback_data: Some(format!("qopen:{token}")),
                    url: None,
                }],
            )
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    drop(guard);
    let text = if rows.is_empty() {
        "No pending questions for you in this topic. Questions interrupted by a restart are not replayed."
    } else {
        "Pending Codex questions. Choose which one to open:"
    };
    let mut outgoing =
        SendMessage::html(message.chat.id, message.message_thread_id, text.to_string());
    outgoing.reply_markup = Some(InlineKeyboardMarkup {
        inline_keyboard: rows.into_iter().map(|r| r.2).collect(),
    });
    shared.telegram.send_message(outgoing).await?;
    Ok(())
}
