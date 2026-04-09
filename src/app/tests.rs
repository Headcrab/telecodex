use super::*;
use crate::config::SearchMode;
use std::path::PathBuf;
use tempfile::NamedTempFile;

fn sample_workspace() -> PathBuf {
    std::env::temp_dir()
        .join("telecodex-tests")
        .join("workspace")
}

fn sample_voice_file() -> PathBuf {
    std::env::temp_dir()
        .join("telecodex-tests")
        .join("attachments")
        .join("voice.ogg")
}

fn sample_turn_workspace() -> TurnWorkspace {
    let root = std::env::temp_dir().join("telecodex-tests").join("turn");
    let out_dir = root.join("out");
    TurnWorkspace { root, out_dir }
}

fn sample_defaults() -> SessionDefaults {
    SessionDefaults {
        cwd: sample_workspace(),
        model: Some("gpt-5.4".to_string()),
        reasoning_effort: Some("medium".to_string()),
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
    }
}

fn sample_turn_request(session_key: SessionKey) -> TurnRequest {
    TurnRequest {
        session_key,
        from_user_id: 100,
        prompt: "hello".to_string(),
        runtime_instructions: None,
        attachments: vec![],
        review_mode: None,
        override_search_mode: None,
    }
}

#[test]
fn detects_stale_codex_thread_errors() {
    let error = anyhow::anyhow!("no rollout found for thread id 019abc | code -32600");

    assert!(should_reset_session_after_error(&error));
}

#[test]
fn detects_stale_codex_thread_errors_in_error_context() {
    let error = anyhow::anyhow!("codex turn failed")
        .context("no rollout found for thread id 019abc | code -32600");

    assert!(should_reset_session_after_error(&error));
}

#[test]
fn ignores_unrelated_invalid_request_errors() {
    let error = anyhow::anyhow!("json-rpc request rejected with code -32600");

    assert!(!should_reset_session_after_error(&error));
}

#[test]
fn validates_absolute_directories() {
    let cwd = std::env::current_dir().unwrap();
    assert!(validate_directory(cwd.to_str().unwrap()).is_ok());
    assert!(validate_directory("relative\\path").is_err());
}

#[test]
fn validates_sandbox_values() {
    assert!(ensure_sandbox_mode("read-only").is_ok());
    assert!(ensure_sandbox_mode("boom").is_err());
}

#[test]
fn enables_live_search_for_latest_queries() {
    assert_eq!(
        auto_search_mode_for_prompt("what's new in the world over the last day?"),
        Some(SearchMode::Live)
    );
    assert_eq!(auto_search_mode_for_prompt("explain this code"), None);
}

#[test]
fn truncates_live_updates_to_single_chunk() {
    let text = "line one\n\nline two\n\nline three";
    let truncated = truncate_for_live_update(text, 16);
    assert!(truncated.len() <= 16);
    assert!(!truncated.is_empty());
}

#[test]
fn hides_sessions_overview_body_when_keyboard_is_available() {
    let session = crate::models::SessionRecord {
        id: 1,
        key: SessionKey::new(-1001234567890, Some(323)),
        session_title: Some("Water meter".to_string()),
        codex_thread_id: Some("019ce152-99e8-7c30-b5b7-166e6aebd550".to_string()),
        force_fresh_thread: false,
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };
    let chat = crate::telegram::Chat {
        id: -1001234567890,
        kind: "supergroup".to_string(),
        is_forum: Some(true),
        username: Some("varv_alarms_bot_chat".to_string()),
        title: Some("Codex chat".to_string()),
    };

    let body = format_sessions_overview(&[session.clone()], session.key, &chat);

    assert_eq!(body, "\u{2063}");
}

#[test]
fn builds_clickable_chat_sessions_keyboard() {
    let session = crate::models::SessionRecord {
        id: 1,
        key: SessionKey::new(-1001234567890, Some(323)),
        session_title: Some("Water meter".to_string()),
        codex_thread_id: Some("019ce152-99e8-7c30-b5b7-166e6aebd550".to_string()),
        force_fresh_thread: false,
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };
    let chat = crate::telegram::Chat {
        id: -1001234567890,
        kind: "supergroup".to_string(),
        is_forum: Some(true),
        username: Some("varv_alarms_bot_chat".to_string()),
        title: Some("Codex chat".to_string()),
    };

    let keyboard = chat_sessions_keyboard(&session, &chat, std::slice::from_ref(&session)).unwrap();

    assert_eq!(
        keyboard.inline_keyboard[0][0].callback_data,
        Some("ses:323".to_string())
    );
    assert_eq!(keyboard.inline_keyboard[0][0].url, None);
}

#[test]
fn builds_topic_links_for_dashboard_root_sessions_keyboard() {
    let root_session = crate::models::SessionRecord {
        id: 1,
        key: SessionKey::new(-1001234567890, None),
        session_title: Some("Dashboard".to_string()),
        codex_thread_id: None,
        force_fresh_thread: false,
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };
    let topic_session = crate::models::SessionRecord {
        id: 2,
        key: SessionKey::new(-1001234567890, Some(323)),
        session_title: Some("Water meter".to_string()),
        codex_thread_id: Some("019ce152-99e8-7c30-b5b7-166e6aebd550".to_string()),
        force_fresh_thread: false,
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };
    let chat = crate::telegram::Chat {
        id: -1001234567890,
        kind: "supergroup".to_string(),
        is_forum: Some(true),
        username: Some("varv_alarms_bot_chat".to_string()),
        title: Some("Codex chat".to_string()),
    };

    let keyboard =
        chat_sessions_keyboard(&root_session, &chat, std::slice::from_ref(&topic_session))
            .unwrap();

    assert_eq!(keyboard.inline_keyboard[0][0].callback_data, None);
    assert_eq!(
        keyboard.inline_keyboard[0][0].url,
        Some("https://t.me/varv_alarms_bot_chat/323?thread=323".to_string())
    );
}

#[test]
fn derives_private_topic_link_slug_from_bot_api_chat_id() {
    assert_eq!(private_topic_link_slug(-1001234567890), Some(1234567890));
    assert_eq!(private_topic_link_slug(275328656), None);
}

#[test]
fn session_environment_match_requires_same_title_and_cwd() {
    let session = crate::models::SessionRecord {
        id: 1,
        key: SessionKey::new(1, Some(10)),
        session_title: Some("Ops Alerts".to_string()),
        codex_thread_id: Some("019".to_string()),
        force_fresh_thread: false,
        updated_at: "2026-03-14T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };

    let same = CodexEnvironmentSummary {
        cwd: environment_identity_for_cwd(&session.cwd),
        name: "Ops Alerts".to_string(),
        latest_thread_id: Some("thr-1".to_string()),
        updated_at: "2026-03-14T10:05:00Z".to_string(),
    };
    let different_title = CodexEnvironmentSummary {
        cwd: environment_identity_for_cwd(&session.cwd),
        name: "ops alerts".to_string(),
        latest_thread_id: Some("thr-2".to_string()),
        updated_at: "2026-03-14T10:06:00Z".to_string(),
    };

    assert!(session_matches_environment(&session, &same));
    assert!(!session_matches_environment(&session, &different_title));
}

#[test]
fn forum_sync_preserves_manual_codex_binding() {
    let session = crate::models::SessionRecord {
        id: 1,
        key: SessionKey::new(-1001234567890, Some(323)),
        session_title: Some("kombez".to_string()),
        codex_thread_id: Some("manual-thread".to_string()),
        force_fresh_thread: false,
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };
    let environment = crate::codex_history::CodexEnvironmentSummary {
        cwd: sample_workspace(),
        name: "kombez".to_string(),
        latest_thread_id: Some("latest-thread".to_string()),
        updated_at: "2026-03-13T10:00:00Z".to_string(),
    };

    assert_eq!(
        super::forum::environment_sync_thread_binding(&session, &environment),
        None
    );
}

#[test]
fn forum_sync_seeds_unbound_environment_session() {
    let session = crate::models::SessionRecord {
        id: 1,
        key: SessionKey::new(-1001234567890, Some(323)),
        session_title: Some("kombez".to_string()),
        codex_thread_id: None,
        force_fresh_thread: false,
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };
    let environment = crate::codex_history::CodexEnvironmentSummary {
        cwd: sample_workspace(),
        name: "kombez".to_string(),
        latest_thread_id: Some("latest-thread".to_string()),
        updated_at: "2026-03-13T10:00:00Z".to_string(),
    };

    assert_eq!(
        super::forum::environment_sync_thread_binding(&session, &environment),
        Some("latest-thread")
    );
}

#[test]
fn forum_sync_preserves_fresh_thread_request() {
    let session = crate::models::SessionRecord {
        id: 1,
        key: SessionKey::new(-1001234567890, Some(323)),
        session_title: Some("kombez".to_string()),
        codex_thread_id: None,
        force_fresh_thread: true,
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };
    let environment = crate::codex_history::CodexEnvironmentSummary {
        cwd: sample_workspace(),
        name: "kombez".to_string(),
        latest_thread_id: Some("latest-thread".to_string()),
        updated_at: "2026-03-13T10:00:00Z".to_string(),
    };

    assert_eq!(
        super::forum::environment_sync_thread_binding(&session, &environment),
        None
    );
}

#[test]
fn picks_last_assistant_text_from_history() {
    let history = vec![
        crate::codex_history::CodexHistoryEntry {
            role: "user".to_string(),
            text: "first".to_string(),
            timestamp: "2026-03-13T09:00:00Z".to_string(),
        },
        crate::codex_history::CodexHistoryEntry {
            role: "assistant".to_string(),
            text: "alpha".to_string(),
            timestamp: "2026-03-13T09:00:01Z".to_string(),
        },
        crate::codex_history::CodexHistoryEntry {
            role: "assistant".to_string(),
            text: "beta".to_string(),
            timestamp: "2026-03-13T09:00:02Z".to_string(),
        },
    ];

    assert_eq!(latest_assistant_text_from_history(&history), Some("beta"));
}

#[test]
fn builds_import_button_for_seed_environment() {
    let session = crate::models::SessionRecord {
        id: 1,
        key: SessionKey::new(-1001234567890, Some(323)),
        session_title: Some("Current topic".to_string()),
        codex_thread_id: Some("019ce152-99e8-7c30-b5b7-166e6aebd550".to_string()),
        force_fresh_thread: false,
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };
    let chat = crate::telegram::Chat {
        id: -1001234567890,
        kind: "supergroup".to_string(),
        is_forum: Some(true),
        username: Some("varv_alarms_bot_chat".to_string()),
        title: Some("Codex chat".to_string()),
    };
    let environment = CodexEnvironmentSummary {
        cwd: sample_workspace().join("seeded"),
        name: "Seeded".to_string(),
        latest_thread_id: None,
        updated_at: String::new(),
    };

    let keyboard = environment_dashboard_keyboard(&chat, &session, &[environment], &[]).unwrap();
    let button = &keyboard.inline_keyboard[0][0];

    assert_eq!(button.url, None);
    assert!(
        button
            .callback_data
            .as_deref()
            .unwrap()
            .starts_with("env:cwd:")
    );
}

#[test]
fn builds_model_quick_commands_from_current_and_default() {
    let commands = model_quick_commands(&[], Some("gpt-5.4"), Some("gpt-5"));

    assert_eq!(
        commands,
        vec![
            vec!["/model gpt-5.4".to_string(), "/model gpt-5".to_string()],
            vec!["/model default".to_string()],
        ]
    );
}

#[test]
fn deduplicates_model_quick_commands_when_current_matches_default() {
    let commands = model_quick_commands(&[], Some("gpt-5.4"), Some("gpt-5.4"));

    assert_eq!(
        commands,
        vec![vec![
            "/model gpt-5.4".to_string(),
            "/model default".to_string(),
        ]]
    );
}

#[test]
fn includes_catalog_models_in_model_quick_commands() {
    let commands = model_quick_commands(
        &[
            AvailableModel {
                id: "gpt-5.4".to_string(),
                display_name: Some("gpt-5.4".to_string()),
                description: None,
                is_default: true,
            },
            AvailableModel {
                id: "gpt-5.3-codex".to_string(),
                display_name: Some("gpt-5.3-codex".to_string()),
                description: None,
                is_default: false,
            },
        ],
        Some("gpt-5.4"),
        None,
    );

    assert_eq!(
        commands,
        vec![
            vec![
                "/model gpt-5.4".to_string(),
                "/model gpt-5.3-codex".to_string(),
            ],
            vec!["/model default".to_string()],
        ]
    );
}

#[test]
fn formats_model_help_text_from_catalog() {
    let text = format_model_help_text(
        "gpt-5.4",
        &[
            AvailableModel {
                id: "gpt-5.4".to_string(),
                display_name: Some("gpt-5.4".to_string()),
                description: None,
                is_default: true,
            },
            AvailableModel {
                id: "gpt-5.3-codex".to_string(),
                display_name: Some("gpt-5.3-codex".to_string()),
                description: None,
                is_default: false,
            },
        ],
    );

    assert!(text.contains("Current model: `gpt-5.4`"));
    assert_eq!(text, "Current model: `gpt-5.4`");
}

#[test]
fn builds_clickable_codex_sessions_keyboard() {
    let session = crate::models::SessionRecord {
        id: 1,
        key: SessionKey::new(1, Some(2)),
        session_title: Some("Telecodex".to_string()),
        codex_thread_id: Some("019ce672-9445-7612-bc5e-c8243a0d1915".to_string()),
        force_fresh_thread: false,
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };
    let summaries = vec![CodexThreadSummary {
        id: "019ce672-9445-7612-bc5e-c8243a0d1915".to_string(),
        title: "Check OpenAI app server".to_string(),
        cwd: sample_workspace(),
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        source: crate::codex_history::CodexHistorySource::Desktop,
    }];

    let keyboard = codex_sessions_keyboard(&session, &summaries).expect("keyboard");

    assert_eq!(
        keyboard.inline_keyboard[0][0].callback_data,
        Some("cmd:/use 019ce672-9445-7612-bc5e-c8243a0d1915".to_string())
    );
    assert_eq!(
        keyboard.inline_keyboard[1][0].callback_data,
        Some("cmd:/use latest".to_string())
    );
    assert_eq!(
        keyboard.inline_keyboard[1][1].callback_data,
        Some("cmd:/clear".to_string())
    );
}

#[test]
fn formats_recent_codex_history_preview() {
    let preview = format_codex_history_preview_plain(&[
        CodexHistoryEntry {
            role: "user".to_string(),
            text: "weather".to_string(),
            timestamp: "2026-03-13T09:00:01Z".to_string(),
        },
        CodexHistoryEntry {
            role: "assistant".to_string(),
            text: "done".to_string(),
            timestamp: "2026-03-13T09:00:03Z".to_string(),
        },
    ]);

    assert!(preview.contains("**Recent Codex History**"));
    assert!(preview.contains("**You**\n│ weather"));
    assert!(preview.contains("**Codex**\n│ done"));
}

#[test]
fn merges_adjacent_history_entries_with_same_role() {
    let preview = format_codex_history_preview_plain(&[
        CodexHistoryEntry {
            role: "assistant".to_string(),
            text: "first answer".to_string(),
            timestamp: "2026-03-13T09:00:01Z".to_string(),
        },
        CodexHistoryEntry {
            role: "assistant".to_string(),
            text: "second answer".to_string(),
            timestamp: "2026-03-13T09:00:02Z".to_string(),
        },
    ]);

    assert!(preview.contains("│ first answer\n│ second answer"));
    assert_eq!(preview.matches("**Codex**").count(), 1);
}

#[test]
fn formats_recent_codex_history_preview_as_html_blockquotes() {
    let preview = format_codex_history_preview_html(&[
        CodexHistoryEntry {
            role: "user".to_string(),
            text: "weather".to_string(),
            timestamp: "2026-03-13T09:00:01Z".to_string(),
        },
        CodexHistoryEntry {
            role: "assistant".to_string(),
            text: "done".to_string(),
            timestamp: "2026-03-13T09:00:03Z".to_string(),
        },
    ]);

    assert!(preview.contains("<b>Recent Codex History</b>"));
    assert!(preview.contains("<b>You</b>\n<blockquote>weather</blockquote>"));
    assert!(preview.contains("<b>Codex</b>\n<blockquote>done</blockquote>"));
}

#[test]
fn preserves_markdown_inside_history_html_blockquotes() {
    let preview = format_codex_history_preview_html(&[CodexHistoryEntry {
        role: "assistant".to_string(),
        text: "Then yes, **counting** is already in progress and there is `code`.".to_string(),
        timestamp: "2026-03-13T09:00:03Z".to_string(),
    }]);

    assert!(preview.contains(
            "<blockquote>Then yes, <b>counting</b> is already in progress and there is <code>code</code>.</blockquote>"
        ));
}

#[test]
fn formats_codex_history_context_for_runtime() {
    let context = format_codex_history_context(&[
        CodexHistoryEntry {
            role: "user".to_string(),
            text: "I need a script".to_string(),
            timestamp: "2026-03-13T09:00:01Z".to_string(),
        },
        CodexHistoryEntry {
            role: "assistant".to_string(),
            text: "working on the script".to_string(),
            timestamp: "2026-03-13T09:00:03Z".to_string(),
        },
    ]);

    assert!(context.contains("Recent conversation context from the selected Codex session"));
    assert!(context.contains("User: I need a script"));
    assert!(context.contains("Assistant: working on the script"));
}

#[test]
fn keeps_audio_transcript_in_user_prompt_only() {
    let voice_path = sample_voice_file();
    let voice_path_display = voice_path.display().to_string();
    let workspace = sample_turn_workspace();
    let out_dir_display = workspace.out_dir.display().to_string();
    let session = crate::models::SessionRecord {
        id: 1,
        key: SessionKey::new(1, Some(2)),
        session_title: Some("Voice notes".to_string()),
        codex_thread_id: None,
        force_fresh_thread: false,
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };
    let request = TurnRequest {
        session_key: session.key,
        from_user_id: 100,
        prompt: "summarize".to_string(),
        runtime_instructions: None,
        attachments: vec![crate::models::LocalAttachment {
            path: voice_path.clone(),
            file_name: "voice.ogg".to_string(),
            mime_type: Some("audio/ogg".to_string()),
            kind: AttachmentKind::Voice,
            transcript: Some(crate::models::AttachmentTranscript {
                engine: "Handy Parakeet".to_string(),
                text: "Hello world".to_string(),
            }),
        }],
        review_mode: None,
        override_search_mode: None,
    };

    let runtime_request = prepare_runtime_request(&session, &request, &workspace);

    assert_eq!(runtime_request.prompt, "summarize\n\nHello world");
    assert!(!runtime_request.prompt.contains(&format!(
        "Attached local files:\n- voice.ogg -> {voice_path_display}"
    )));
    assert!(
        !runtime_request
            .prompt
            .contains("If you generate final deliverable files for the user")
    );
    assert!(!runtime_request.prompt.contains(&voice_path_display));
    let runtime_instructions = runtime_request.runtime_instructions.unwrap();
    assert!(runtime_instructions.contains(&out_dir_display));
    assert!(!runtime_instructions.contains(&voice_path_display));
}

#[test]
fn keeps_non_transcribed_audio_paths_in_user_prompt() {
    let voice_path = sample_voice_file();
    let voice_path_display = voice_path.display().to_string();
    let workspace = sample_turn_workspace();
    let session = crate::models::SessionRecord {
        id: 1,
        key: SessionKey::new(1, Some(2)),
        session_title: Some("Voice notes".to_string()),
        codex_thread_id: None,
        force_fresh_thread: false,
        updated_at: "2026-03-13T10:00:00Z".to_string(),
        cwd: sample_workspace(),
        model: None,
        reasoning_effort: None,
        session_prompt: None,
        sandbox_mode: "workspace-write".to_string(),
        approval_policy: "never".to_string(),
        search_mode: SearchMode::Disabled,
        add_dirs: vec![],
        busy: false,
    };
    let request = TurnRequest {
        session_key: session.key,
        from_user_id: 100,
        prompt: "Analyze the attached files.".to_string(),
        runtime_instructions: None,
        attachments: vec![crate::models::LocalAttachment {
            path: voice_path.clone(),
            file_name: "voice.ogg".to_string(),
            mime_type: Some("audio/ogg".to_string()),
            kind: AttachmentKind::Voice,
            transcript: None,
        }],
        review_mode: None,
        override_search_mode: None,
    };

    let runtime_request = prepare_runtime_request(&session, &request, &workspace);

    assert!(
        runtime_request
            .prompt
            .contains("Local files for this turn:")
    );
    assert!(
        runtime_request
            .prompt
            .contains(&format!("voice.ogg -> {voice_path_display}"))
    );
    assert!(
        runtime_request
            .runtime_instructions
            .unwrap()
            .contains(&format!(
                "Attached local files:\n- voice.ogg -> {voice_path_display}"
            ))
    );
}

#[test]
fn parses_approval_callback_payloads() {
    assert_eq!(
        parse_approval_callback_data("apr:abc123:a"),
        Some(("abc123".to_string(), CodexApprovalDecision::Accept))
    );
    assert_eq!(
        parse_approval_callback_data("apr:abc123:s"),
        Some((
            "abc123".to_string(),
            CodexApprovalDecision::AcceptForSession
        ))
    );
    assert_eq!(parse_approval_callback_data("cmd:/help"), None);
}

#[test]
fn builds_approval_keyboard_buttons() {
    let keyboard = approval_keyboard(
        "token123",
        &[
            CodexApprovalDecision::Accept,
            CodexApprovalDecision::Decline,
            CodexApprovalDecision::Cancel,
        ],
    )
    .expect("approval keyboard");

    assert_eq!(keyboard.inline_keyboard.len(), 2);
    assert_eq!(
        keyboard.inline_keyboard[0][0].callback_data,
        Some("apr:token123:a".to_string())
    );
    assert_eq!(
        keyboard.inline_keyboard[0][1].callback_data,
        Some("apr:token123:d".to_string())
    );
    assert_eq!(
        keyboard.inline_keyboard[1][0].callback_data,
        Some("apr:token123:c".to_string())
    );
}

#[test]
fn derives_session_title_from_first_non_empty_line() {
    assert_eq!(
        derive_session_title_from_text("\n  Check OpenAI app server   \nsecond line"),
        Some("Check OpenAI app server".to_string())
    );
}

#[test]
fn truncates_long_session_titles() {
    let title = derive_session_title_from_text(
        "Check a very long session title so the Telegram layout stays readable and does not break",
    )
    .expect("title");
    assert!(title.ends_with('…'));
    assert!(title.chars().count() <= 48);
}

#[test]
fn detects_commands_that_use_session_context() {
    assert!(command_uses_session_context(&ParsedInput::Forward(
        "/help".to_string()
    )));
    assert!(command_uses_session_context(&ParsedInput::Bridge(
        BridgeCommand::Copy
    )));
    assert!(!command_uses_session_context(&ParsedInput::Bridge(
        BridgeCommand::Sessions
    )));
    assert!(!command_uses_session_context(&ParsedInput::Bridge(
        BridgeCommand::Status
    )));
    assert!(!command_uses_session_context(&ParsedInput::Bridge(
        BridgeCommand::RestartBot
    )));
}

#[test]
fn detects_commands_that_require_codex_auth() {
    assert!(!parsed_input_requires_codex_auth(&ParsedInput::Bridge(
        BridgeCommand::Status
    )));
    assert!(parsed_input_requires_codex_auth(&ParsedInput::Bridge(
        BridgeCommand::Review(crate::models::ReviewRequest {
            base: None,
            commit: None,
            uncommitted: true,
            title: None,
            prompt: None,
        })
    )));
    assert!(!parsed_input_requires_codex_auth(&ParsedInput::Bridge(
        BridgeCommand::Pwd
    )));
    assert!(!parsed_input_requires_codex_auth(&ParsedInput::Bridge(
        BridgeCommand::Login
    )));
}

#[tokio::test]
async fn upload_failure_marks_turn_failed_and_cleanup_still_runs() {
    let tmp = NamedTempFile::new().unwrap();
    let store = Store::open(tmp.path(), &[100], &sample_defaults()).unwrap();
    let session = store
        .ensure_session(SessionKey::new(1, Some(2)), 100, &sample_defaults())
        .unwrap();
    let turn_id = store
        .record_turn_started(session.id, &sample_turn_request(session.key))
        .unwrap();

    let attachment_dir = tempfile::tempdir().unwrap();
    let attachment_path = attachment_dir.path().join("input.txt");
    std::fs::write(&attachment_path, "payload").unwrap();
    let turn_root = attachment_dir.path().join("turn-root");
    std::fs::create_dir_all(&turn_root).unwrap();

    let attachment = LocalAttachment {
        path: attachment_path.clone(),
        file_name: "input.txt".to_string(),
        mime_type: Some("text/plain".to_string()),
        kind: AttachmentKind::Text,
        transcript: None,
    };
    let summary = crate::codex::RunSummary {
        codex_thread_id: Some("thread-123".to_string()),
        assistant_text: "answer".to_string(),
        stderr_text: String::new(),
    };
    let failure_messages = Arc::new(StdMutex::new(Vec::<String>::new()));
    let failure_messages_sink = failure_messages.clone();

    let result = finalize_foreground_turn(
        ForegroundTurnSuccess {
            store: &store,
            session: &session,
            turn_id,
            review_mode: false,
            summary: &summary,
        },
        || async { Err(anyhow!("upload failed")) },
        || async { Ok(()) },
        move |message| {
            let failure_messages_sink = failure_messages_sink.clone();
            async move {
                failure_messages_sink.lock().unwrap().push(message);
                Ok(())
            }
        },
    )
    .await;
    let result = finish_turn_cleanup(&[attachment], &turn_root, result);

    assert!(result.is_err());
    assert_eq!(
        store.turn_status(turn_id).unwrap().as_deref(),
        Some("failed")
    );
    assert!(!attachment_path.exists());
    assert!(!turn_root.exists());
    assert!(
        failure_messages
            .lock()
            .unwrap()
            .iter()
            .any(|message| message.contains("upload failed"))
    );
}
