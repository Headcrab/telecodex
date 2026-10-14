use std::{
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result, anyhow};
use chrono::{Duration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::{
    config::{CodexConfig, SearchMode},
    models::{
        CollaborationMode, ReviewRequest, SessionKey, SessionRecord, TurnRequest, UserRecord,
        UserRole,
    },
};

pub const INSTANCE_LOCK_LOST_ERROR: &str = "Telecodex database instance lock was lost";

pub struct Store {
    conn: Mutex<Connection>,
    instance_id: String,
}

#[derive(Debug, Clone)]
pub struct SessionDefaults {
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub session_prompt: Option<String>,
    pub sandbox_mode: String,
    pub approval_policy: String,
    pub search_mode: SearchMode,
    pub add_dirs: Vec<PathBuf>,
}

impl From<&CodexConfig> for SessionDefaults {
    fn from(value: &CodexConfig) -> Self {
        Self {
            cwd: value.default_cwd.clone(),
            model: value.default_model.clone(),
            reasoning_effort: value.default_reasoning_effort.clone(),
            session_prompt: None,
            sandbox_mode: value.default_sandbox.clone(),
            approval_policy: value.default_approval.clone(),
            search_mode: value.default_search_mode,
            add_dirs: value.default_add_dirs.clone(),
        }
    }
}

impl Store {
    pub fn open(db_path: &Path, admin_ids: &[i64], defaults: &SessionDefaults) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("failed to open sqlite database {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let instance_id = Uuid::now_v7().to_string();
        let store = Self {
            conn: Mutex::new(conn),
            instance_id,
        };
        store.init_schema()?;
        let stale_cutoff = stale_instance_cutoff();
        store.claim_instance_lock(&stale_cutoff)?;
        // A restart does not authorize replaying a partially dispatched turn.
        // Keep its input available and surface uncertainty for a human retry.
        {
            let conn = store.conn.lock().expect("store mutex poisoned");
            conn.execute(
                "UPDATE incoming_updates SET status='undetermined',updated_at=?1
                 WHERE status IN ('received','processing','queued','steering','answering','started')
                 AND instance_id!=?2",
                params![now_string(), store.instance_id],
            )?;
        }
        let recovered = store.recover_interrupted_turns(&stale_cutoff)?;
        store.seed_admins(admin_ids)?;
        store.audit(
            None,
            "startup",
            serde_json::json!({
                "admins_seeded": admin_ids,
                "interrupted_turns_recovered": recovered.interrupted_turns,
                "busy_sessions_recovered": recovered.busy_sessions,
            }),
        )?;
        if recovered.interrupted_turns > 0 || recovered.busy_sessions > 0 {
            tracing::warn!(
                "recovered {} interrupted turns and {} busy sessions from a previous process",
                recovered.interrupted_turns,
                recovered.busy_sessions
            );
        }
        if admin_ids.is_empty() {
            tracing::warn!(
                "startup_admin_ids is empty; the bot will deny everyone until an admin is inserted manually"
            );
        }
        if defaults.cwd.as_os_str().is_empty() {
            return Err(anyhow!("default cwd is empty"));
        }
        Ok(store)
    }

    fn claim_instance_lock(&self, stale_cutoff: &str) -> Result<()> {
        let now = now_string();
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "INSERT INTO app_instance_lock(key, instance_id, heartbeat_at, acquired_at)
             VALUES ('main', ?1, ?2, ?2)
             ON CONFLICT(key) DO UPDATE
             SET instance_id = excluded.instance_id,
                 heartbeat_at = excluded.heartbeat_at,
                 acquired_at = excluded.acquired_at
             WHERE app_instance_lock.instance_id = excluded.instance_id
                OR app_instance_lock.heartbeat_at < ?3",
            params![self.instance_id, now, stale_cutoff],
        )?;
        if changed != 0 {
            return Ok(());
        }

        let holder = conn
            .query_row(
                "SELECT instance_id, heartbeat_at FROM app_instance_lock WHERE key = 'main'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((holder_id, heartbeat_at)) = holder else {
            return Err(anyhow!("failed to claim Telecodex database instance lock"));
        };
        Err(anyhow!(
            "Telecodex database is already owned by live instance {holder_id} with heartbeat {heartbeat_at}"
        ))
    }

    pub fn heartbeat_instance(&self) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE app_instance_lock
             SET heartbeat_at = ?2
             WHERE key = 'main' AND instance_id = ?1",
            params![self.instance_id, now_string()],
        )?;
        if changed == 0 {
            return Err(anyhow!(INSTANCE_LOCK_LOST_ERROR));
        }
        Ok(())
    }

    pub fn release_instance_lock(&self) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        release_instance_lock_for(&conn, &self.instance_id)
    }

    fn recover_interrupted_turns(&self, stale_cutoff: &str) -> Result<InterruptedTurnRecovery> {
        let now = now_string();
        let conn = self.conn.lock().expect("store mutex poisoned");
        let interrupted_turns = conn.execute(
            "UPDATE turns
             SET status = 'failed',
                 assistant_text = COALESCE(assistant_text, 'Telecodex restarted before this turn completed. Retry the request if it still matters.'),
                 completed_at = ?1
             WHERE status = 'running' AND started_at < ?2",
            params![now, stale_cutoff],
        )?;
        let busy_sessions = conn.execute(
            "UPDATE sessions
             SET busy = 0, updated_at = ?1
             WHERE busy != 0 AND updated_at < ?2",
            params![now, stale_cutoff],
        )?;
        Ok(InterruptedTurnRecovery {
            interrupted_turns,
            busy_sessions,
        })
    }

    pub fn last_update_id(&self) -> Result<Option<i64>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT value FROM bot_state WHERE key = 'last_update_id'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| value.parse::<i64>().map_err(|error| anyhow!(error)))
        .transpose()
    }

    /// Admit input and its Telegram acknowledgement boundary atomically.
    /// A duplicate ID never dispatches the same input a second time.
    pub fn admit_update(&self, update: &crate::telegram::Update) -> Result<bool> {
        let payload = serde_json::to_string(update)?;
        if payload.len() > 1_048_576 {
            return Err(anyhow!("telegram update exceeds ingress journal bound"));
        }
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM incoming_updates WHERE update_id=?1",
                params![update.update_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != payload {
                return Err(anyhow!("telegram update ID content conflict"));
            }
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO incoming_updates(update_id,payload_json,status,instance_id,updated_at)
             VALUES(?1,?2,'received',?3,?4)",
            params![update.update_id, payload, self.instance_id, now_string()],
        )?;
        tx.execute(
            "INSERT INTO bot_state(key,value) VALUES('last_update_id',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value
             WHERE CAST(bot_state.value AS INTEGER)<CAST(excluded.value AS INTEGER)",
            params![update.update_id.to_string()],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn begin_update(&self, update_id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE incoming_updates SET status='processing',updated_at=?2
             WHERE update_id=?1 AND status='received' AND instance_id=?3",
            params![update_id, now_string(), self.instance_id],
        )?;
        if changed != 1 {
            return Err(anyhow!("telegram update is not available for dispatch"));
        }
        Ok(())
    }

    pub fn begin_question_answer(
        &self,
        update_id: i64,
        turn_id: i64,
        request_id: &crate::user_input::NativeRequestId,
    ) -> Result<()> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;
        let changed=tx.execute(
            "UPDATE incoming_updates SET status='answering',turn_id=?2,updated_at=?3 WHERE update_id=?1 AND status='processing' AND instance_id=?4",
            params![update_id,turn_id,now_string(),self.instance_id],
        )?;
        if changed != 1 {
            return Err(anyhow!("question input cannot be answered twice"));
        }
        tx.execute(
            "INSERT INTO question_answers(update_id,turn_id,request_id) VALUES(?1,?2,?3)",
            params![update_id, turn_id, serde_json::to_string(request_id)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn resolve_question_answers(
        &self,
        turn_id: i64,
        request_id: &crate::user_input::NativeRequestId,
        submitted: bool,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "UPDATE incoming_updates SET status=CASE WHEN ?3=0 THEN 'undetermined' ELSE
             CASE (SELECT status FROM turns WHERE id=?1) WHEN 'running' THEN 'started'
             WHEN 'completed' THEN 'settled' ELSE 'undetermined' END END,updated_at=?4
             WHERE status='answering' AND instance_id=?5 AND update_id IN
             (SELECT update_id FROM question_answers WHERE turn_id=?1 AND request_id=?2)",
            params![
                turn_id,
                serde_json::to_string(request_id)?,
                submitted,
                now_string(),
                self.instance_id
            ],
        )?;
        Ok(())
    }

    pub fn mark_update_queued(&self, update_id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE incoming_updates SET status='queued',updated_at=?2
             WHERE update_id=?1 AND status='processing' AND instance_id=?3",
            params![update_id, now_string(), self.instance_id],
        )?;
        if changed != 1 {
            return Err(anyhow!("telegram update cannot be queued twice"));
        }
        Ok(())
    }

    pub fn link_update_turn(&self, update_id: i64, turn_id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE incoming_updates SET status='started',turn_id=?2,updated_at=?3
             WHERE update_id=?1 AND status='queued' AND instance_id=?4",
            params![update_id, turn_id, now_string(), self.instance_id],
        )?;
        if changed != 1 {
            return Err(anyhow!("telegram update cannot start another turn"));
        }
        Ok(())
    }

    pub fn finish_update_dispatch(&self, update_id: i64, success: bool) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "UPDATE incoming_updates SET status=?2,updated_at=?3
             WHERE update_id=?1 AND instance_id=?4 AND status='processing'",
            params![
                update_id,
                if success { "handled" } else { "undetermined" },
                now_string(),
                self.instance_id
            ],
        )?;
        Ok(())
    }

    /// Save the active turn link before an input can cross the native transport.
    pub fn begin_update_steer(&self, update_id: i64, turn_id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE incoming_updates SET status='steering',turn_id=?2,updated_at=?3
             WHERE update_id=?1 AND status='processing' AND instance_id=?4",
            params![update_id, turn_id, now_string(), self.instance_id],
        )?;
        if changed != 1 {
            return Err(anyhow!("telegram input cannot attempt another steer"));
        }
        Ok(())
    }

    /// A definite rejection allows the same admitted input to enter the normal queue.
    pub fn reject_update_steer(&self, update_id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE incoming_updates SET status='processing',turn_id=NULL,updated_at=?2
             WHERE update_id=?1 AND status='steering' AND instance_id=?3",
            params![update_id, now_string(), self.instance_id],
        )?;
        if changed != 1 {
            return Err(anyhow!(
                "telegram steer is not available for queue fallback"
            ));
        }
        Ok(())
    }

    /// The turn may finish between acknowledgement and recording acceptance.
    pub fn accept_update_steer(&self, update_id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE incoming_updates SET status=CASE
               (SELECT status FROM turns WHERE id=incoming_updates.turn_id)
               WHEN 'running' THEN 'started' WHEN 'completed' THEN 'settled'
               ELSE 'undetermined' END,updated_at=?2
             WHERE update_id=?1 AND status='steering' AND instance_id=?3",
            params![update_id, now_string(), self.instance_id],
        )?;
        if changed != 1 {
            return Err(anyhow!("telegram steer cannot be accepted twice"));
        }
        Ok(())
    }

    pub fn mark_update_steer_undetermined(&self, update_id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "UPDATE incoming_updates SET status='undetermined',updated_at=?2
             WHERE update_id=?1 AND status='steering' AND instance_id=?3",
            params![update_id, now_string(), self.instance_id],
        )?;
        Ok(())
    }

    pub fn finish_update_turn(&self, update_id: i64, success: bool) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "UPDATE incoming_updates SET status=?2,updated_at=?3
             WHERE update_id=?1 AND instance_id=?4 AND status IN ('queued','started')",
            params![
                update_id,
                if success { "settled" } else { "undetermined" },
                now_string(),
                self.instance_id
            ],
        )?;
        Ok(())
    }

    pub fn uncertain_update_ids(&self, user_id: i64, key: SessionKey) -> Result<Vec<i64>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut query = conn.prepare(
            "SELECT update_id FROM incoming_updates WHERE status='undetermined'
             AND COALESCE(json_extract(payload_json,'$.message.from.id'),
                          json_extract(payload_json,'$.callback_query.from.id'))=?1
             AND COALESCE(json_extract(payload_json,'$.message.chat.id'),
                          json_extract(payload_json,'$.callback_query.message.chat.id'))=?2
             AND COALESCE(json_extract(payload_json,'$.message.message_thread_id'),
                          json_extract(payload_json,'$.callback_query.message.message_thread_id'),0)=?3
             ORDER BY update_id DESC LIMIT 20",
        )?;
        Ok(query
            .query_map(params![user_id, key.chat_id, key.thread_id], |row| {
                row.get(0)
            })?
            .collect::<rusqlite::Result<Vec<i64>>>()?)
    }

    pub fn bot_state_value(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT value FROM bot_state WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn save_bot_state(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO bot_state(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_user(&self, tg_user_id: i64) -> Result<Option<UserRecord>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT tg_user_id, role, allowed FROM users WHERE tg_user_id = ?1",
            params![tg_user_id],
            |row| {
                let role: String = row.get(1)?;
                Ok(UserRecord {
                    tg_user_id: row.get(0)?,
                    role: UserRole::try_from(role.as_str()).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(io::Error::new(
                                io::ErrorKind::InvalidData,
                                error.to_string(),
                            )),
                        )
                    })?,
                    allowed: row.get::<_, i64>(2)? != 0,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn upsert_user(&self, tg_user_id: i64, role: UserRole, allowed: bool) -> Result<()> {
        let now = now_string();
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO users(tg_user_id, role, allowed, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(tg_user_id) DO UPDATE
             SET role = excluded.role, allowed = excluded.allowed, updated_at = excluded.updated_at",
            params![tg_user_id, role.as_str(), bool_to_i64(allowed), now],
        )?;
        Ok(())
    }

    pub fn ensure_session(
        &self,
        key: SessionKey,
        creator_user_id: i64,
        defaults: &SessionDefaults,
    ) -> Result<SessionRecord> {
        if let Some(existing) = self.get_session(key)? {
            return Ok(existing);
        }

        let now = now_string();
        let add_dirs = serde_json::to_string(&path_vec_to_strings(&defaults.add_dirs))?;
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO sessions(
                chat_id, thread_id, session_title, codex_thread_id, force_fresh_thread, cwd, model, reasoning_effort, session_prompt, sandbox_mode, approval_policy,
                search_mode, add_dirs_json, creator_user_id, busy, last_assistant_text, created_at, updated_at
             ) VALUES (?1, ?2, NULL, NULL, 0, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, NULL, ?12, ?12)",
            params![
                key.chat_id,
                key.thread_id,
                defaults.cwd.display().to_string(),
                defaults.model,
                defaults.reasoning_effort,
                defaults.session_prompt,
                defaults.sandbox_mode,
                defaults.approval_policy,
                defaults.search_mode.as_codex_value(),
                add_dirs,
                creator_user_id,
                now
            ],
        )?;
        drop(conn);
        self.get_session(key)?
            .ok_or_else(|| anyhow!("failed to reload newly created session"))
    }

    pub fn get_session(&self, key: SessionKey) -> Result<Option<SessionRecord>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT id, chat_id, thread_id, session_title, codex_thread_id, force_fresh_thread, cwd, model, reasoning_effort, session_prompt, sandbox_mode, approval_policy,
                    search_mode, add_dirs_json, creator_user_id, busy, last_assistant_text, updated_at, service_tier, collaboration_mode
             FROM sessions
             WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id],
            map_session,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_chat_sessions(&self, chat_id: i64) -> Result<Vec<SessionRecord>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, chat_id, thread_id, session_title, codex_thread_id, force_fresh_thread, cwd, model, reasoning_effort, session_prompt, sandbox_mode, approval_policy,
                    search_mode, add_dirs_json, creator_user_id, busy, last_assistant_text, updated_at, service_tier, collaboration_mode
             FROM sessions
             WHERE chat_id = ?1
             ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![chat_id], map_session)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn delete_session(&self, key: SessionKey) -> Result<()> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM sessions WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id],
        )?;
        tx.execute(
            "DELETE FROM bot_state WHERE key=?1",
            params![format!(
                "session_title_owner:{}:{}",
                key.chat_id, key.thread_id
            )],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_session_busy(&self, key: SessionKey, busy: bool) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET busy = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, bool_to_i64(busy), now_string()],
        )
    }

    pub fn set_session_codex_thread(&self, key: SessionKey, codex_thread_id: &str) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET codex_thread_id = ?3, force_fresh_thread = 0, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, codex_thread_id, now_string()],
        )
    }

    pub fn set_session_title(&self, key: SessionKey, session_title: Option<&str>) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET session_title = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, session_title, now_string()],
        )
    }

    pub fn explicit_session_title(&self, key: SessionKey) -> Result<Option<String>> {
        self.bot_state_value(&format!(
            "session_title_owner:{}:{}",
            key.chat_id, key.thread_id
        ))
    }

    pub fn set_explicit_session_title(&self, key: SessionKey, title: &str) -> Result<()> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET session_title=?3, updated_at=?4 WHERE chat_id=?1 AND thread_id=?2",
            params![key.chat_id, key.thread_id, title, now_string()],
        )?;
        if changed != 1 {
            return Err(anyhow!("session not found"));
        }
        tx.execute(
            "INSERT INTO bot_state(key,value) VALUES (?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![
                format!("session_title_owner:{}:{}", key.chat_id, key.thread_id),
                title
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn apply_session_template(&self, key: SessionKey, template: &SessionRecord) -> Result<()> {
        let add_dirs = serde_json::to_string(&path_vec_to_strings(&template.add_dirs))?;
        self.update_session_field(
            key,
            "UPDATE sessions
             SET session_title = ?3,
                 codex_thread_id = ?4,
                 force_fresh_thread = ?5,
                 cwd = ?6,
                 model = ?7,
                 reasoning_effort = ?8,
                 session_prompt = ?9,
                 sandbox_mode = ?10,
                 approval_policy = ?11,
                 search_mode = ?12,
                 add_dirs_json = ?13,
                 service_tier = ?14,
                 collaboration_mode = ?15,
                 updated_at = ?16
             WHERE chat_id = ?1 AND thread_id = ?2",
            params![
                key.chat_id,
                key.thread_id,
                template.session_title,
                template.codex_thread_id,
                bool_to_i64(template.force_fresh_thread),
                template.cwd.display().to_string(),
                template.model,
                template.reasoning_effort,
                template.session_prompt,
                template.sandbox_mode,
                template.approval_policy,
                template.search_mode.as_codex_value(),
                add_dirs,
                template.service_tier.as_deref(),
                template.collaboration_mode.map(CollaborationMode::as_str),
                now_string(),
            ],
        )
    }

    pub fn clear_session_conversation(&self, key: SessionKey) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions
             SET codex_thread_id = NULL, force_fresh_thread = 1, busy = 0, last_assistant_text = NULL, updated_at = ?3
             WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, now_string()],
        )
    }

    pub fn set_session_cwd(&self, key: SessionKey, cwd: &Path) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET cwd = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![
                key.chat_id,
                key.thread_id,
                cwd.display().to_string(),
                now_string()
            ],
        )
    }

    pub fn set_session_model(&self, key: SessionKey, model: Option<&str>) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET model = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, model, now_string()],
        )
    }

    pub fn set_session_reasoning_effort(
        &self,
        key: SessionKey,
        reasoning_effort: Option<&str>,
    ) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET reasoning_effort = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, reasoning_effort, now_string()],
        )
    }

    pub fn set_session_service_tier(
        &self,
        key: SessionKey,
        service_tier: Option<&str>,
    ) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET service_tier = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, service_tier, now_string()],
        )
    }

    pub fn set_session_collaboration_mode(
        &self,
        key: SessionKey,
        mode: CollaborationMode,
    ) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET collaboration_mode = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, mode.as_str(), now_string()],
        )
    }

    pub fn record_question_message(
        &self,
        key: SessionKey,
        message_id: i64,
        turn_id: i64,
        correlation: &serde_json::Value,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO question_messages(chat_id, thread_id, message_id, turn_id, correlation_json) VALUES (?1,?2,?3,?4,?5)",
            params![key.chat_id, key.thread_id, message_id, turn_id, correlation.to_string()],
        )?;
        Ok(())
    }

    pub fn is_question_message(&self, key: SessionKey, message_id: i64) -> Result<bool> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        Ok(conn.query_row(
            "SELECT 1 FROM question_messages WHERE chat_id=?1 AND thread_id=?2 AND message_id=?3",
            params![key.chat_id, key.thread_id, message_id], |_| Ok(true),
        ).optional()?.unwrap_or(false))
    }

    pub fn set_session_prompt(&self, key: SessionKey, session_prompt: Option<&str>) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET session_prompt = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, session_prompt, now_string()],
        )
    }

    pub fn set_session_approval(&self, key: SessionKey, approval: &str) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET approval_policy = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, approval, now_string()],
        )
    }

    pub fn set_session_sandbox(&self, key: SessionKey, sandbox: &str) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET sandbox_mode = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, sandbox, now_string()],
        )
    }

    pub fn set_session_search_mode(&self, key: SessionKey, mode: SearchMode) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET search_mode = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, mode.as_codex_value(), now_string()],
        )
    }

    pub fn add_session_dir(&self, key: SessionKey, path: &Path) -> Result<Vec<PathBuf>> {
        let mut session = self
            .get_session(key)?
            .ok_or_else(|| anyhow!("session not found"))?;
        if !session.add_dirs.iter().any(|existing| existing == path) {
            session.add_dirs.push(path.to_path_buf());
        }
        let json = serde_json::to_string(&path_vec_to_strings(&session.add_dirs))?;
        self.update_session_field(
            key,
            "UPDATE sessions SET add_dirs_json = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, json, now_string()],
        )?;
        Ok(session.add_dirs)
    }

    pub fn record_turn_started(&self, session_id: i64, request: &TurnRequest) -> Result<i64> {
        let review_json = serde_json::to_string(&request.review_mode)?;
        let now = now_string();
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO turns(session_id, from_user_id, prompt, review_json, status, started_at, completed_at, assistant_text)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, NULL, NULL)",
            params![session_id, request.from_user_id, request.prompt, review_json, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn record_turn_finished(
        &self,
        turn_id: i64,
        status: &str,
        assistant_text: Option<&str>,
    ) -> Result<()> {
        let now = now_string();
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE turns
             SET status = ?2, assistant_text = COALESCE(?3, assistant_text), completed_at = ?4
             WHERE id = ?1",
            params![turn_id, status, assistant_text, now],
        )?;
        // An acknowledged steer shares this turn's outcome. Unacknowledged attempts
        // remain uncertain even if the original turn completes successfully.
        tx.execute(
            "UPDATE incoming_updates SET status='undetermined',updated_at=?2 WHERE turn_id=?1 AND status='answering' AND instance_id=?3",
            params![turn_id,now,self.instance_id],
        )?;
        tx.execute(
            "UPDATE incoming_updates SET status=?2,updated_at=?3
             WHERE turn_id=?1 AND status='started' AND instance_id=?4",
            params![
                turn_id,
                if status == "completed" {
                    "settled"
                } else {
                    "undetermined"
                },
                now,
                self.instance_id
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn retry_request_for_turn(
        &self,
        turn_id: i64,
        session_key: SessionKey,
        from_user_id: i64,
    ) -> Result<Option<TurnRequest>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let row = conn
            .query_row(
                "SELECT t.prompt, t.review_json, t.status
                 FROM turns t
                 JOIN sessions s ON s.id = t.session_id
                 WHERE t.id = ?1 AND s.chat_id = ?2 AND s.thread_id = ?3",
                params![turn_id, session_key.chat_id, session_key.thread_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((prompt, review_json, status)) = row else {
            return Ok(None);
        };
        if !matches!(status.as_str(), "failed" | "cancelled") {
            return Ok(None);
        }
        let review_mode: Option<ReviewRequest> = match review_json {
            Some(review_json) => serde_json::from_str(&review_json)
                .with_context(|| format!("failed to parse review request for turn {turn_id}"))?,
            None => None,
        };
        Ok(Some(TurnRequest {
            session_key,
            from_user_id,
            prompt,
            runtime_instructions: None,
            attachments: vec![],
            review_mode,
            override_search_mode: None,
        }))
    }

    pub fn set_last_assistant_text(&self, key: SessionKey, text: Option<&str>) -> Result<()> {
        self.update_session_field(
            key,
            "UPDATE sessions SET last_assistant_text = ?3, updated_at = ?4 WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id, text, now_string()],
        )
    }

    pub fn last_assistant_text(&self, key: SessionKey) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT last_assistant_text FROM sessions WHERE chat_id = ?1 AND thread_id = ?2",
            params![key.chat_id, key.thread_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|value| value.flatten())
        .map_err(Into::into)
    }

    pub fn audit(
        &self,
        actor_user_id: Option<i64>,
        action: &str,
        details: serde_json::Value,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO audit_log(actor_user_id, action, details_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![actor_user_id, action, details.to_string(), now_string()],
        )?;
        Ok(())
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS users(
                tg_user_id INTEGER PRIMARY KEY,
                role TEXT NOT NULL,
                allowed INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                chat_id INTEGER NOT NULL,
                thread_id INTEGER NOT NULL,
                session_title TEXT,
                codex_thread_id TEXT,
                force_fresh_thread INTEGER NOT NULL DEFAULT 0,
                cwd TEXT NOT NULL,
                model TEXT,
                reasoning_effort TEXT,
                service_tier TEXT,
                collaboration_mode TEXT,
                session_prompt TEXT,
                sandbox_mode TEXT NOT NULL,
                approval_policy TEXT NOT NULL,
                search_mode TEXT NOT NULL,
                add_dirs_json TEXT NOT NULL,
                creator_user_id INTEGER NOT NULL,
                busy INTEGER NOT NULL DEFAULT 0,
                last_assistant_text TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(chat_id, thread_id)
            );

            CREATE TABLE IF NOT EXISTS turns(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL,
                from_user_id INTEGER NOT NULL,
                prompt TEXT NOT NULL,
                review_json TEXT,
                status TEXT NOT NULL,
                assistant_text TEXT,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS bot_state(
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS question_messages(
                chat_id INTEGER NOT NULL,
                thread_id INTEGER NOT NULL,
                message_id INTEGER NOT NULL,
                turn_id INTEGER NOT NULL,
                correlation_json TEXT NOT NULL,
                PRIMARY KEY(chat_id, message_id),
                FOREIGN KEY(turn_id) REFERENCES turns(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS incoming_updates(
                update_id INTEGER PRIMARY KEY,
                payload_json TEXT NOT NULL,
                status TEXT NOT NULL,
                instance_id TEXT NOT NULL,
                turn_id INTEGER,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS question_answers(
                update_id INTEGER PRIMARY KEY,
                turn_id INTEGER NOT NULL,
                request_id TEXT NOT NULL,
                FOREIGN KEY(update_id) REFERENCES incoming_updates(update_id) ON DELETE CASCADE,
                FOREIGN KEY(turn_id) REFERENCES turns(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS audit_log(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                actor_user_id INTEGER,
                action TEXT NOT NULL,
                details_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS app_instance_lock(
                key TEXT PRIMARY KEY,
                instance_id TEXT NOT NULL,
                heartbeat_at TEXT NOT NULL,
                acquired_at TEXT NOT NULL
            );
            ",
        )?;
        add_column_if_missing(&conn, "sessions", "session_title", "TEXT")?;
        add_column_if_missing(&conn, "sessions", "reasoning_effort", "TEXT")?;
        add_column_if_missing(&conn, "sessions", "service_tier", "TEXT")?;
        add_column_if_missing(&conn, "sessions", "collaboration_mode", "TEXT")?;
        add_column_if_missing(&conn, "sessions", "session_prompt", "TEXT")?;
        add_column_if_missing(
            &conn,
            "sessions",
            "force_fresh_thread",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        Ok(())
    }

    fn seed_admins(&self, admin_ids: &[i64]) -> Result<()> {
        for admin_id in admin_ids {
            self.upsert_user(*admin_id, UserRole::Admin, true)?;
        }
        Ok(())
    }

    fn update_session_field<P>(&self, key: SessionKey, sql: &str, params: P) -> Result<()>
    where
        P: rusqlite::Params,
    {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(sql, params)?;
        if changed == 0 {
            return Err(anyhow!(
                "session not found for chat_id={} thread_id={}",
                key.chat_id,
                key.thread_id
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn turn_status(&self, turn_id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT status FROM turns WHERE id = ?1",
            params![turn_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Ok(conn) = self.conn.get_mut() {
            let _ = release_instance_lock_for(conn, &self.instance_id);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InterruptedTurnRecovery {
    interrupted_turns: usize,
    busy_sessions: usize,
}

fn map_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    let add_dirs_json: String = row.get(13)?;
    let add_dirs_strings: Vec<String> = serde_json::from_str(&add_dirs_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(13, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let search_mode: String = row.get(12)?;
    Ok(SessionRecord {
        id: row.get(0)?,
        key: SessionKey {
            chat_id: row.get(1)?,
            thread_id: row.get(2)?,
        },
        session_title: row.get(3)?,
        codex_thread_id: row.get(4)?,
        force_fresh_thread: row.get::<_, i64>(5)? != 0,
        updated_at: row.get(17)?,
        cwd: normalize_path(PathBuf::from(row.get::<_, String>(6)?)),
        model: row.get(7)?,
        reasoning_effort: row.get(8)?,
        service_tier: row.get(18)?,
        collaboration_mode: row
            .get::<_, Option<String>>(19)?
            .map(|mode| {
                CollaborationMode::try_from(mode.as_str())
                    .map_err(|_| rusqlite::Error::InvalidQuery)
            })
            .transpose()?,
        session_prompt: row.get(9)?,
        sandbox_mode: row.get(10)?,
        approval_policy: row.get(11)?,
        search_mode: match search_mode.as_str() {
            "live" => SearchMode::Live,
            "cached" => SearchMode::Cached,
            _ => SearchMode::Disabled,
        },
        add_dirs: add_dirs_strings
            .into_iter()
            .map(PathBuf::from)
            .map(normalize_path)
            .collect(),
        busy: row.get::<_, i64>(15)? != 0,
    })
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&pragma)?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if columns.iter().any(|existing| existing == column) {
        return Ok(());
    }

    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )?;
    Ok(())
}

fn now_string() -> String {
    Utc::now().to_rfc3339()
}

fn release_instance_lock_for(conn: &Connection, instance_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM app_instance_lock WHERE key = 'main' AND instance_id = ?1",
        params![instance_id],
    )?;
    Ok(())
}

fn stale_instance_cutoff() -> String {
    let seconds = std::env::var("TELECODEX_INSTANCE_STALE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(120);
    (Utc::now() - Duration::seconds(seconds)).to_rfc3339()
}

fn bool_to_i64(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn path_vec_to_strings(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

fn normalize_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let raw = path.as_os_str().to_string_lossy();
        if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = raw.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::*;
    use crate::models::ReviewRequest;

    fn input(update_id: i64, user_id: i64, text: &str) -> crate::telegram::Update {
        serde_json::from_value(serde_json::json!({
            "update_id":update_id,"message":{
                "message_id":update_id,"message_thread_id":7,
                "from":{"id":user_id,"is_bot":false,"first_name":"Test"},
                "chat":{"id":user_id,"type":"private"},"text":text
            }
        }))
        .unwrap()
    }

    #[test]
    fn uncertain_inputs_are_scoped_to_human_chat_and_topic() {
        let tmp = NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        let mut inputs = vec![
            input(1, 100, "this topic"),
            input(2, 100, "other topic"),
            input(3, 100, "other chat"),
            input(4, 200, "other human"),
        ];
        inputs[1].message.as_mut().unwrap().message_thread_id = Some(8);
        inputs[2].message.as_mut().unwrap().chat.id = 200;
        inputs[3].message.as_mut().unwrap().chat.id = 100;
        for update in inputs {
            store.admit_update(&update).unwrap();
            store.begin_update(update.update_id).unwrap();
            store
                .finish_update_dispatch(update.update_id, false)
                .unwrap();
        }
        let callback = serde_json::from_value(serde_json::json!({
            "update_id":5,"callback_query":{"id":"cb","from":{"id":100,"is_bot":false,"first_name":"Test"},
                "data":"q:expired:0:0","message":{"message_id":50,"message_thread_id":7,"chat":{"id":100,"type":"private"}}}
        })).unwrap();
        store.admit_update(&callback).unwrap();
        store.begin_update(5).unwrap();
        store.finish_update_dispatch(5, false).unwrap();
        assert_eq!(
            store
                .uncertain_update_ids(100, SessionKey::new(100, Some(7)))
                .unwrap(),
            vec![5, 1]
        );
        assert_eq!(
            store
                .uncertain_update_ids(100, SessionKey::new(100, Some(8)))
                .unwrap(),
            vec![2]
        );
        assert_eq!(
            store
                .uncertain_update_ids(100, SessionKey::new(200, Some(7)))
                .unwrap(),
            vec![3]
        );
        assert_eq!(
            store
                .uncertain_update_ids(200, SessionKey::new(100, Some(7)))
                .unwrap(),
            vec![4]
        );
    }

    #[test]
    fn input_admission_is_atomic_idempotent_and_does_not_rewind_offset() {
        let tmp = NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        let first = input(40, 100, "one");
        assert!(store.admit_update(&first).unwrap());
        assert_eq!(store.last_update_id().unwrap(), Some(40));
        assert!(!store.admit_update(&first).unwrap());
        assert!(store.admit_update(&input(39, 100, "older")).unwrap());
        assert_eq!(store.last_update_id().unwrap(), Some(40));
        assert!(store.admit_update(&input(40, 100, "changed")).is_err());
        store.begin_update(40).unwrap();
        assert!(store.begin_update(40).is_err());
        // An actual SQLite write fault cannot acknowledge absent input.
        {
            let conn = store.conn.lock().unwrap();
            conn.execute_batch(
                "CREATE TRIGGER refuse_input BEFORE INSERT ON incoming_updates
                BEGIN SELECT RAISE(ABORT, 'synthetic admission fault'); END;",
            )
            .unwrap();
        }
        assert!(store.admit_update(&input(41, 100, "fault")).is_err());
        assert_eq!(store.last_update_id().unwrap(), Some(40));
        let conn = store.conn.lock().unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM incoming_updates WHERE update_id=41",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn restart_preserves_interrupted_input_without_automatic_replay() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
            store
                .admit_update(&input(1, 100, "before dispatch"))
                .unwrap();
            store.admit_update(&input(2, 100, "queued")).unwrap();
            store.begin_update(2).unwrap();
            store.mark_update_queued(2).unwrap();
            store.admit_update(&input(3, 200, "other human")).unwrap();
            store.admit_update(&input(4, 100, "completed")).unwrap();
            store.begin_update(4).unwrap();
            store.finish_update_dispatch(4, true).unwrap();
        }
        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        assert_eq!(store.last_update_id().unwrap(), Some(4));
        assert_eq!(
            store
                .uncertain_update_ids(100, SessionKey::new(100, Some(7)))
                .unwrap(),
            vec![2, 1]
        );
        assert_eq!(
            store
                .uncertain_update_ids(200, SessionKey::new(200, Some(7)))
                .unwrap(),
            vec![3]
        );
        assert!(!store.admit_update(&input(2, 100, "queued")).unwrap());
        assert!(store.begin_update(2).is_err());
        let conn = store.conn.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM turns", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let original: String = conn
            .query_row(
                "SELECT payload_json FROM incoming_updates WHERE update_id=2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&original).unwrap()["message"]["text"],
            "queued"
        );
    }

    #[test]
    fn input_turn_link_cannot_be_replaced_or_finished_by_dispatcher() {
        let tmp = NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        store.admit_update(&input(1, 100, "turn")).unwrap();
        store.begin_update(1).unwrap();
        store.mark_update_queued(1).unwrap();
        store.link_update_turn(1, 42).unwrap();
        assert!(store.link_update_turn(1, 43).is_err());
        store.finish_update_dispatch(1, true).unwrap();
        {
            let conn = store.conn.lock().unwrap();
            let row: (String, i64) = conn
                .query_row(
                    "SELECT status,turn_id FROM incoming_updates WHERE update_id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(row, ("started".to_string(), 42));
        }
        store.finish_update_turn(1, false).unwrap();
        assert_eq!(
            store
                .uncertain_update_ids(100, SessionKey::new(100, Some(7)))
                .unwrap(),
            vec![1]
        );
    }

    #[test]
    fn acknowledged_steers_share_outcome_even_when_ack_is_recorded_after_completion() {
        for (status, expected) in [("completed", "settled"), ("failed", "undetermined")] {
            for late_ack in [false, true] {
                let tmp = NamedTempFile::new().unwrap();
                let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
                let key = SessionKey::new(100, Some(7));
                let session = store.ensure_session(key, 100, &defaults()).unwrap();
                let request = TurnRequest {
                    session_key: key,
                    from_user_id: 100,
                    prompt: "first".to_string(),
                    runtime_instructions: None,
                    attachments: vec![],
                    review_mode: None,
                    override_search_mode: None,
                };
                let turn_id = store.record_turn_started(session.id, &request).unwrap();
                for id in [1, 2, 3] {
                    store
                        .admit_update(&input(id, 100, "additional input"))
                        .unwrap();
                    store.begin_update(id).unwrap();
                    store.begin_update_steer(id, turn_id).unwrap();
                }
                if !late_ack {
                    store.accept_update_steer(1).unwrap();
                }
                store.record_turn_finished(turn_id, status, None).unwrap();
                if late_ack {
                    store.accept_update_steer(1).unwrap();
                }
                // Completion cannot confirm that an unacknowledged input was accepted.
                store.mark_update_steer_undetermined(2).unwrap();
                // A never-sent or definitely rejected input may still enter the queue.
                store.reject_update_steer(3).unwrap();
                store.mark_update_queued(3).unwrap();
                store.finish_update_dispatch(1, true).unwrap();
                let conn = store.conn.lock().unwrap();
                let states: Vec<(String, Option<i64>)> = conn
                    .prepare("SELECT status,turn_id FROM incoming_updates ORDER BY update_id")
                    .unwrap()
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                assert_eq!(
                    states,
                    vec![
                        (expected.to_string(), Some(turn_id)),
                        ("undetermined".to_string(), Some(turn_id)),
                        ("queued".to_string(), None)
                    ]
                );
            }
        }
    }

    #[test]
    fn restart_preserves_an_unacknowledged_steer_and_its_turn_link() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
            store
                .admit_update(&input(1, 100, "possibly transmitted"))
                .unwrap();
            store.begin_update(1).unwrap();
            store.begin_update_steer(1, 42).unwrap();
        }
        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        assert_eq!(
            store
                .uncertain_update_ids(100, SessionKey::new(100, Some(7)))
                .unwrap(),
            vec![1]
        );
        let conn = store.conn.lock().unwrap();
        let turn: i64 = conn
            .query_row(
                "SELECT turn_id FROM incoming_updates WHERE update_id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(turn, 42);
    }

    fn defaults() -> SessionDefaults {
        SessionDefaults {
            cwd: std::env::current_dir().unwrap(),
            model: Some("gpt-5.4".to_string()),
            reasoning_effort: Some("medium".to_string()),
            session_prompt: None,
            sandbox_mode: "read-only".to_string(),
            approval_policy: "never".to_string(),
            search_mode: SearchMode::Disabled,
            add_dirs: vec![],
        }
    }

    fn mark_turn_and_session_stale(db_path: &std::path::Path, turn_id: i64, key: SessionKey) {
        let stale = (Utc::now() - Duration::seconds(300)).to_rfc3339();
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute(
            "UPDATE turns SET started_at = ?1 WHERE id = ?2",
            rusqlite::params![stale, turn_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE chat_id = ?2 AND thread_id = ?3",
            rusqlite::params![stale, key.chat_id, key.thread_id],
        )
        .unwrap();
    }

    #[test]
    fn seeds_admins_and_sessions() {
        let tmp = NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        let admin = store.get_user(100).unwrap().unwrap();
        assert!(admin.allowed);
        assert_eq!(admin.role, UserRole::Admin);

        let session = store
            .ensure_session(SessionKey::new(1, Some(10)), 100, &defaults())
            .unwrap();
        assert_eq!(session.key.thread_id, 10);
    }

    #[test]
    fn records_turns() {
        let tmp = NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        let session = store
            .ensure_session(SessionKey::new(1, Some(10)), 100, &defaults())
            .unwrap();
        let request = TurnRequest {
            session_key: session.key,
            from_user_id: 100,
            prompt: "hello".to_string(),
            runtime_instructions: None,
            attachments: vec![],
            review_mode: Some(ReviewRequest {
                base: Some("main".to_string()),
                commit: None,
                uncommitted: false,
                title: None,
                prompt: None,
            }),
            override_search_mode: None,
        };
        let turn_id = store.record_turn_started(session.id, &request).unwrap();
        store
            .record_turn_finished(turn_id, "completed", Some("answer"))
            .unwrap();
        let last = store.last_assistant_text(session.key).unwrap();
        assert!(last.is_none());
    }

    #[test]
    fn recovers_interrupted_turns_on_startup() {
        let tmp = NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        let session = store
            .ensure_session(SessionKey::new(1, Some(10)), 100, &defaults())
            .unwrap();
        let request = TurnRequest {
            session_key: session.key,
            from_user_id: 100,
            prompt: "hello".to_string(),
            runtime_instructions: None,
            attachments: vec![],
            review_mode: None,
            override_search_mode: None,
        };
        let turn_id = store.record_turn_started(session.id, &request).unwrap();
        store.set_session_busy(session.key, true).unwrap();
        drop(store);
        mark_turn_and_session_stale(tmp.path(), turn_id, session.key);

        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        let session = store.get_session(session.key).unwrap().unwrap();

        assert!(!session.busy);
        assert_eq!(
            store.turn_status(turn_id).unwrap().as_deref(),
            Some("failed")
        );
    }

    #[test]
    fn keeps_fresh_running_turns_on_startup() {
        let tmp = NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        let session = store
            .ensure_session(SessionKey::new(1, Some(10)), 100, &defaults())
            .unwrap();
        let request = TurnRequest {
            session_key: session.key,
            from_user_id: 100,
            prompt: "hello".to_string(),
            runtime_instructions: None,
            attachments: vec![],
            review_mode: None,
            override_search_mode: None,
        };
        let turn_id = store.record_turn_started(session.id, &request).unwrap();
        store.set_session_busy(session.key, true).unwrap();
        drop(store);

        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        let session = store.get_session(session.key).unwrap().unwrap();

        assert!(session.busy);
        assert_eq!(
            store.turn_status(turn_id).unwrap().as_deref(),
            Some("running")
        );
    }

    #[test]
    fn refuses_second_live_instance() {
        let tmp = NamedTempFile::new().unwrap();
        let _store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        let second = Store::open(tmp.path(), &[100], &defaults());

        assert!(second.is_err());
        assert!(format!("{:#}", second.err().unwrap()).contains("already owned by live instance"));
    }

    #[test]
    fn builds_retry_request_for_failed_turn() {
        let tmp = NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        let session = store
            .ensure_session(SessionKey::new(1, Some(10)), 100, &defaults())
            .unwrap();
        let request = TurnRequest {
            session_key: session.key,
            from_user_id: 100,
            prompt: "hello".to_string(),
            runtime_instructions: None,
            attachments: vec![],
            review_mode: Some(ReviewRequest {
                base: Some("main".to_string()),
                commit: None,
                uncommitted: false,
                title: None,
                prompt: Some("review this".to_string()),
            }),
            override_search_mode: None,
        };
        let turn_id = store.record_turn_started(session.id, &request).unwrap();
        store.record_turn_finished(turn_id, "failed", None).unwrap();

        let retry = store
            .retry_request_for_turn(turn_id, session.key, 200)
            .unwrap()
            .unwrap();

        assert_eq!(retry.from_user_id, 200);
        assert_eq!(retry.prompt, "hello");
        assert!(retry.attachments.is_empty());
        assert_eq!(retry.review_mode.unwrap().base.as_deref(), Some("main"));
    }

    #[test]
    fn builds_retry_request_for_failed_turn_with_null_review_json() {
        let tmp = NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path(), &[100], &defaults()).unwrap();
        let session = store
            .ensure_session(SessionKey::new(1, Some(10)), 100, &defaults())
            .unwrap();
        let request = TurnRequest {
            session_key: session.key,
            from_user_id: 100,
            prompt: "hello".to_string(),
            runtime_instructions: None,
            attachments: vec![],
            review_mode: None,
            override_search_mode: None,
        };
        let turn_id = store.record_turn_started(session.id, &request).unwrap();
        store.record_turn_finished(turn_id, "failed", None).unwrap();
        {
            let conn = store.conn.lock().expect("store mutex poisoned");
            conn.execute(
                "UPDATE turns SET review_json = NULL WHERE id = ?1",
                rusqlite::params![turn_id],
            )
            .unwrap();
        }

        let retry = store
            .retry_request_for_turn(turn_id, session.key, 200)
            .unwrap()
            .unwrap();

        assert_eq!(retry.from_user_id, 200);
        assert_eq!(retry.prompt, "hello");
        assert!(retry.review_mode.is_none());
    }
}
