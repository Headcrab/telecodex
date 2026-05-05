use crate::{
    config::SearchMode,
    limits::{LimitsSnapshot, default_codex_home},
    models::{SessionRecord, TurnRequest},
};
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    task::JoinHandle,
    time::{Duration, Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct CodexRunner {
    binary: PathBuf,
}
pub struct RunSummary {
    pub codex_thread_id: Option<String>,
    pub assistant_text: String,
    pub stderr_text: String,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AvailableModel {
    pub id: String,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "isDefault")]
    pub is_default: bool,
}
#[derive(Debug, Clone)]
pub enum CodexEvent {
    Progress(String),
    AssistantText(String),
    ThreadStarted(String),
    ApprovalRequest(CodexApprovalRequest),
}

#[derive(Debug, Clone)]
pub struct CodexApprovalRequest {
    pub kind: CodexApprovalKind,
    pub prompt: String,
    pub options: Vec<CodexApprovalDecision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexApprovalKind {
    CommandExecution,
    FileChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexApprovalDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexEventOutcome {
    None,
    Approval(CodexApprovalDecision),
}
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub current_dir: Option<PathBuf>,
}
struct AppServerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout_lines: tokio::io::Lines<BufReader<ChildStdout>>,
    stderr_buffer: Arc<Mutex<String>>,
    stderr_task: JoinHandle<()>,
    next_id: u64,
}
enum RpcMessage {
    Response {
        id: u64,
        result: Option<Value>,
        error: Option<RpcError>,
    },
    Notification {
        method: String,
        params: Value,
    },
    ServerRequest {
        id: u64,
        method: String,
        params: Value,
    },
}
#[derive(Debug, Deserialize)]
struct RpcError {
    #[serde(default)]
    code: Option<i64>,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}
#[derive(Debug, Deserialize)]
struct ModelListPage {
    #[serde(default)]
    data: Vec<AvailableModel>,
    #[serde(default, rename = "nextCursor")]
    next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAuthStatus {
    pub authenticated: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexDeviceAuthPrompt {
    pub verification_uri: String,
    pub user_code: String,
}

pub struct CodexDeviceAuthSession {
    child: Child,
    stdout_lines: tokio::io::Lines<BufReader<ChildStdout>>,
    stdout_buffer: String,
    stderr_buffer: Arc<Mutex<String>>,
    stderr_task: Option<JoinHandle<()>>,
}

struct SimpleCommandOutput {
    success: bool,
    message: String,
}

impl CodexRunner {
    pub fn new(binary: PathBuf) -> Self {
        Self { binary }
    }
    pub fn build_review_command(
        &self,
        session: &SessionRecord,
        request: &TurnRequest,
    ) -> Option<CommandSpec> {
        request
            .review_mode
            .as_ref()
            .map(|review| build_review_command(&self.binary, session, request, review))
    }
    pub async fn auth_status(&self) -> Result<CodexAuthStatus> {
        if has_api_key_auth(&default_codex_home())? {
            return Ok(CodexAuthStatus {
                authenticated: true,
                detail: "Using API key authentication.".to_string(),
            });
        }
        let output = run_simple_command_capture(&self.binary, &["login", "status"]).await?;
        interpret_auth_status(output)
    }
    pub async fn logout(&self) -> Result<String> {
        run_simple_command(&self.binary, &["logout"]).await
    }
    pub async fn start_device_auth(&self) -> Result<CodexDeviceAuthSession> {
        let spec = CommandSpec {
            program: self.binary.clone(),
            args: vec!["login".to_string(), "--device-auth".to_string()],
            current_dir: None,
        };
        let mut command = spawnable_command(&spec);
        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to spawn codex command: {} {}",
                spec.program.display(),
                spec.args.join(" ")
            )
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("codex stdout pipe unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("codex stderr pipe unavailable"))?;
        let stderr_buffer = Arc::new(Mutex::new(String::new()));
        let stderr_buffer_task = stderr_buffer.clone();
        let stderr_task = tokio::spawn(async move {
            let mut stderr_lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = stderr_lines.next_line().await {
                let clean = strip_ansi_codes(&line);
                let mut buffer = stderr_buffer_task.lock().await;
                append_output_line(&mut buffer, &clean);
                tracing::debug!("codex auth stderr: {clean}");
            }
        });
        Ok(CodexDeviceAuthSession {
            child,
            stdout_lines: BufReader::new(stdout).lines(),
            stdout_buffer: String::new(),
            stderr_buffer,
            stderr_task: Some(stderr_task),
        })
    }
    pub async fn read_rate_limits(&self) -> Result<Option<LimitsSnapshot>> {
        let mut process = AppServerProcess::spawn(&self.binary).await?;
        process.initialize().await?;
        let request_id = process
            .send_request("account/rateLimits/read", Value::Null)
            .await?;
        let response = process.await_response(request_id).await?;
        let stderr_text = process.shutdown().await?;
        if !stderr_text.is_empty() {
            tracing::debug!("codex rate-limits stderr: {stderr_text}");
        }
        let snapshot = response
            .get("rateLimits")
            .cloned()
            .or_else(|| response.get("rate_limits").cloned());
        snapshot
            .map(serde_json::from_value)
            .transpose()
            .context("failed to parse rate limits response")
    }
    pub async fn read_models(&self) -> Result<Vec<AvailableModel>> {
        let mut process = AppServerProcess::spawn(&self.binary).await?;
        process.initialize().await?;
        let mut models = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = model_list_params(cursor.as_deref());
            let request_id = process.send_request("model/list", params).await?;
            let response = process.await_response(request_id).await?;
            let page: ModelListPage =
                serde_json::from_value(response).context("failed to parse model list response")?;
            models.extend(page.data);
            if let Some(next_cursor) = page
                .next_cursor
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
            {
                cursor = Some(next_cursor);
            } else {
                break;
            }
        }
        let stderr_text = process.shutdown().await?;
        if !stderr_text.is_empty() {
            tracing::debug!("codex model-list stderr: {stderr_text}");
        }
        Ok(models)
    }
    pub async fn run_turn<F, Fut>(
        &self,
        session: &SessionRecord,
        request: &TurnRequest,
        cancel: CancellationToken,
        mut on_event: F,
    ) -> Result<RunSummary>
    where
        F: FnMut(CodexEvent) -> Fut,
        Fut: std::future::Future<Output = Result<CodexEventOutcome>>,
    {
        if let Some(spec) = self.build_review_command(session, request) {
            return run_review_turn(spec, cancel, on_event).await;
        }
        run_app_server_turn(&self.binary, session, request, cancel, &mut on_event).await
    }
}

impl CodexDeviceAuthSession {
    pub async fn read_prompt(&mut self) -> Result<CodexDeviceAuthPrompt> {
        loop {
            let Some(line) = self
                .stdout_lines
                .next_line()
                .await
                .context("reading codex login stdout failed")?
            else {
                let status = self
                    .child
                    .wait()
                    .await
                    .context("waiting for codex login failed")?;
                if let Some(stderr_task) = self.stderr_task.take() {
                    let _ = stderr_task.await;
                }
                let stderr = self.stderr_buffer.lock().await.trim().to_string();
                if stderr.is_empty() {
                    bail!("codex login exited with status {status} before emitting a device code");
                }
                bail!("{stderr}");
            };
            let clean = strip_ansi_codes(&line);
            append_output_line(&mut self.stdout_buffer, &clean);
            if let Some(prompt) = parse_device_auth_prompt(&self.stdout_buffer) {
                return Ok(prompt);
            }
        }
    }

    pub async fn wait(mut self, cancel: CancellationToken) -> Result<String> {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    terminate_child(&mut self.child).await;
                    if let Some(stderr_task) = self.stderr_task.take() {
                        let _ = stderr_task.await;
                    }
                    bail!("codex login cancelled");
                }
                next_line = self.stdout_lines.next_line() => {
                    match next_line.context("reading codex login stdout failed")? {
                        Some(line) => {
                            let clean = strip_ansi_codes(&line);
                            append_output_line(&mut self.stdout_buffer, &clean);
                        }
                        None => break,
                    }
                }
            }
        }
        let status = self
            .child
            .wait()
            .await
            .context("waiting for codex login failed")?;
        if let Some(stderr_task) = self.stderr_task.take() {
            let _ = stderr_task.await;
        }
        let stdout = self.stdout_buffer.trim().to_string();
        let stderr = self.stderr_buffer.lock().await.trim().to_string();
        if !status.success() {
            if !stderr.is_empty() {
                bail!("codex login exited with status {status}: {stderr}");
            }
            if !stdout.is_empty() {
                bail!("codex login exited with status {status}: {stdout}");
            }
            bail!("codex login exited with status {status}");
        }
        Ok(stdout)
    }
}

async fn run_app_server_turn<F, Fut>(
    binary: &Path,
    session: &SessionRecord,
    request: &TurnRequest,
    cancel: CancellationToken,
    on_event: &mut F,
) -> Result<RunSummary>
where
    F: FnMut(CodexEvent) -> Fut,
    Fut: std::future::Future<Output = Result<CodexEventOutcome>>,
{
    let mut process = AppServerProcess::spawn(binary).await?;
    process.initialize().await?;
    let thread_id = process.start_or_resume_thread(session, request).await?;
    let mut summary = RunSummary {
        codex_thread_id: Some(thread_id.clone()),
        assistant_text: String::new(),
        stderr_text: String::new(),
    };
    let _ = on_event(CodexEvent::ThreadStarted(thread_id.clone())).await?;
    let turn_request_id = process
        .send_request(
            "turn/start",
            build_turn_start_params(&thread_id, session, request),
        )
        .await?;
    let mut active_turn_id: Option<String> = None;
    let mut interrupt_sent = false;
    let mut cancel_deadline: Option<Instant> = None;
    let mut turn_error: Option<String> = None;
    let mut cancelled = false;
    let mut turn_completed = false;
    let mut assistant_message_completed = false;
    while !turn_completed {
        tokio::select! {
          _=cancel.cancelled(), if !interrupt_sent => {
            if let Some(turn_id)=active_turn_id.as_deref(){process.send_request("turn/interrupt",json!({"threadId":thread_id,"turnId":turn_id})).await?; interrupt_sent=true; cancelled=true; cancel_deadline=Some(Instant::now()+Duration::from_secs(5)); let _ = on_event(CodexEvent::Progress("Interrupt requested.".to_string())).await?;} else {cancelled=true; break;}
          }
          _=async{if let Some(deadline)=cancel_deadline{sleep_until(deadline).await;}}, if cancel_deadline.is_some() => {cancelled=true; break;}
          next_message=process.next_message()=>{
            let Some(message)=next_message? else {break;};
            match message{
              RpcMessage::Response{id,result,error}=>{
                if id==turn_request_id{if let Some(error)=error{turn_error=Some(format_rpc_error(&error)); break;} if let Some(turn_id)=result.as_ref().and_then(|v|v.get("turn")).and_then(|t|t.get("id")).and_then(Value::as_str){active_turn_id=Some(turn_id.to_string());}}
                else if error.is_some() && interrupt_sent {turn_error=Some(format!("turn/interrupt failed: {}",format_rpc_error(error.as_ref().expect("interrupt error missing")))); break;}
              }
              RpcMessage::Notification{method,params}=>{handle_notification(&method,&params,&mut summary,&mut active_turn_id,&mut turn_error,&mut turn_completed,&mut assistant_message_completed,on_event).await?;}
                RpcMessage::ServerRequest{id,method,params}=>{handle_server_request(&mut process,id,&method,&params,on_event).await?;}
            }
          }
        }
    }
    summary.stderr_text = process.shutdown().await?;
    if let Some(error) = turn_error {
        bail!("{error}");
    }
    if cancelled {
        bail!("codex turn cancelled");
    }
    Ok(summary)
}

async fn handle_notification<F, Fut>(
    method: &str,
    params: &Value,
    summary: &mut RunSummary,
    active_turn_id: &mut Option<String>,
    turn_error: &mut Option<String>,
    turn_completed: &mut bool,
    assistant_message_completed: &mut bool,
    on_event: &mut F,
) -> Result<()>
where
    F: FnMut(CodexEvent) -> Fut,
    Fut: std::future::Future<Output = Result<CodexEventOutcome>>,
{
    match method {
        "thread/started" => {
            if let Some(thread_id) = params
                .get("thread")
                .and_then(|t| t.get("id"))
                .and_then(Value::as_str)
            {
                summary.codex_thread_id = Some(thread_id.to_string());
                let _ = on_event(CodexEvent::ThreadStarted(thread_id.to_string())).await?;
            }
        }
        "turn/started" => {
            if let Some(turn_id) = params
                .get("turn")
                .and_then(|t| t.get("id"))
                .and_then(Value::as_str)
            {
                *active_turn_id = Some(turn_id.to_string());
                *assistant_message_completed = false;
            }
        }
        "turn/completed" => {
            if let Some(turn) = params.get("turn") {
                let status = turn
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed");
                if status == "failed" {
                    *turn_error = Some(
                        turn.get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("turn failed")
                            .to_string(),
                    );
                } else if status == "interrupted" {
                    *turn_error = Some("codex turn cancelled".to_string());
                }
                *active_turn_id = None;
                *turn_completed = true;
            }
        }
        "thread/status/changed" => {
            let is_idle = params
                .get("status")
                .and_then(|status| status.get("type"))
                .and_then(Value::as_str)
                == Some("idle");
            if is_idle
                && active_turn_id.is_some()
                && (*assistant_message_completed || turn_error.is_some())
            {
                *active_turn_id = None;
                *turn_completed = true;
            }
        }
        "item/agentMessage/delta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                summary.assistant_text.push_str(delta);
                let _ = on_event(CodexEvent::AssistantText(summary.assistant_text.clone())).await?;
            }
        }
        "item/started" => {
            if let Some(item) = params.get("item") {
                if item.get("type").and_then(Value::as_str) == Some("commandExecution") {
                    let command = item
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or("command");
                    let _ = on_event(CodexEvent::Progress(format!("Running `{command}`"))).await?;
                }
            }
        }
        "item/completed" => {
            if let Some(item) = params.get("item") {
                match item.get("type").and_then(Value::as_str) {
                    Some("agentMessage") => {
                        let text = item
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        *assistant_message_completed = true;
                        summary.assistant_text = text.clone();
                        let _ = on_event(CodexEvent::AssistantText(text)).await?;
                        if item.get("phase").and_then(Value::as_str) == Some("final_answer") {
                            *active_turn_id = None;
                            *turn_completed = true;
                        }
                    }
                    Some("commandExecution") => {
                        let status = item
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("completed");
                        let command = item
                            .get("command")
                            .and_then(Value::as_str)
                            .unwrap_or("command");
                        let output = item
                            .get("aggregatedOutput")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        let text = if output.is_empty() {
                            format!("{command} {status}")
                        } else {
                            format!("{command} {status}\n{output}")
                        };
                        let _ = on_event(CodexEvent::Progress(text)).await?;
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    Ok(())
}

async fn handle_server_request<F, Fut>(
    process: &mut AppServerProcess,
    id: u64,
    method: &str,
    params: &Value,
    on_event: &mut F,
) -> Result<()>
where
    F: FnMut(CodexEvent) -> Fut,
    Fut: std::future::Future<Output = Result<CodexEventOutcome>>,
{
    match method {
        "item/commandExecution/requestApproval" => {
            let request = build_command_approval_request(params);
            let outcome = on_event(CodexEvent::ApprovalRequest(request)).await?;
            process
                .send_result(
                    id,
                    json!({"decision": approval_decision_value(outcome_to_approval_decision(outcome))}),
                )
                .await?;
        }
        "item/fileChange/requestApproval" => {
            let request = build_file_change_approval_request(params);
            let outcome = on_event(CodexEvent::ApprovalRequest(request)).await?;
            process
                .send_result(
                    id,
                    json!({"decision": approval_decision_value(outcome_to_approval_decision(outcome))}),
                )
                .await?;
        }
        "item/tool/requestUserInput" => {
            process.send_result(id, json!({"answers":{}})).await?;
            let _ = on_event(CodexEvent::Progress(
                "Tool requested user input, but Telegram replies are not wired yet.".to_string(),
            ))
            .await?;
        }
        _ => {
            bail!("unsupported app-server server request `{method}`");
        }
    }
    Ok(())
}
async fn run_review_turn<F, Fut>(
    spec: CommandSpec,
    cancel: CancellationToken,
    mut on_event: F,
) -> Result<RunSummary>
where
    F: FnMut(CodexEvent) -> Fut,
    Fut: std::future::Future<Output = Result<CodexEventOutcome>>,
{
    let mut command = spawnable_command(&spec);
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to spawn codex command: {} {}",
            spec.program.display(),
            spec.args.join(" ")
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("codex stdout pipe unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("codex stderr pipe unavailable"))?;
    let stderr_buffer = Arc::new(Mutex::new(String::new()));
    let stderr_buffer_task = stderr_buffer.clone();
    let stderr_task = tokio::spawn(async move {
        let mut stderr_lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = stderr_lines.next_line().await {
            let mut buffer = stderr_buffer_task.lock().await;
            if !buffer.is_empty() {
                buffer.push('\n');
            }
            buffer.push_str(&line);
            tracing::warn!("codex stderr: {line}");
        }
    });
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut summary = RunSummary {
        codex_thread_id: None,
        assistant_text: String::new(),
        stderr_text: String::new(),
    };
    loop {
        tokio::select! {
          _=cancel.cancelled()=>{terminate_child(&mut child).await; let _=stderr_task.await; bail!("codex turn cancelled");}
          next_line=stdout_lines.next_line()=>{match next_line.context("reading codex stdout failed")?{Some(line)=>{if let Some(event)=parse_exec_event(&line)?{match &event{CodexEvent::ThreadStarted(thread_id)=>summary.codex_thread_id=Some(thread_id.clone()),CodexEvent::AssistantText(text)=>summary.assistant_text=text.clone(),CodexEvent::Progress(_)|CodexEvent::ApprovalRequest(_)=>{}} let _ = on_event(event).await?;}},None=>break,}}
        }
    }
    let status = child.wait().await.context("waiting for codex failed")?;
    let _ = stderr_task.await;
    summary.stderr_text = stderr_buffer.lock().await.trim().to_string();
    if !status.success() {
        if summary.stderr_text.is_empty() {
            bail!("codex exited with status {status}");
        } else {
            bail!("codex exited with status {status}: {}", summary.stderr_text);
        }
    }
    Ok(summary)
}

fn build_review_command(
    binary: &Path,
    session: &SessionRecord,
    request: &TurnRequest,
    review: &crate::models::ReviewRequest,
) -> CommandSpec {
    let effective_search_mode = request.override_search_mode.unwrap_or(session.search_mode);
    let developer_instructions = merge_instruction_sections(
        session.session_prompt.as_deref(),
        request.runtime_instructions.as_deref(),
    );
    let mut args = vec![
        "exec".to_string(),
        "review".to_string(),
        "--json".to_string(),
        "--skip-git-repo-check".to_string(),
    ];
    push_common_config_args(
        &mut args,
        effective_search_mode,
        &session.approval_policy,
        &session.model,
        session.reasoning_effort.as_deref(),
        developer_instructions.as_deref(),
    );
    if review.uncommitted {
        args.push("--uncommitted".to_string());
    }
    if let Some(base) = &review.base {
        args.push("--base".to_string());
        args.push(base.clone());
    }
    if let Some(commit) = &review.commit {
        args.push("--commit".to_string());
        args.push(commit.clone());
    }
    if let Some(title) = &review.title {
        args.push("--title".to_string());
        args.push(title.clone());
    }
    if let Some(prompt) = &review.prompt {
        args.push(prompt.clone());
    } else if !request.prompt.is_empty() {
        args.push(request.prompt.clone());
    }
    CommandSpec {
        program: binary.to_path_buf(),
        args,
        current_dir: Some(session.cwd.clone()),
    }
}

fn build_app_server_command(binary: &Path) -> CommandSpec {
    CommandSpec {
        program: binary.to_path_buf(),
        args: vec!["app-server".to_string()],
        current_dir: None,
    }
}

fn build_thread_request(session: &SessionRecord, request: &TurnRequest) -> (&'static str, Value) {
    let developer_instructions = merge_instruction_sections(
        session.session_prompt.as_deref(),
        request.runtime_instructions.as_deref(),
    );
    let params = json!({"threadId":session.codex_thread_id,"model":session.model,"cwd":sanitize_arg_path(&session.cwd),"approvalPolicy":session.approval_policy,"sandbox":session.sandbox_mode,"config":build_config_overrides(session.search_mode),"serviceName":"telecodex","developerInstructions":developer_instructions});
    if session.codex_thread_id.is_some() {
        ("thread/resume", params)
    } else {
        ("thread/start", params)
    }
}

fn build_turn_start_params(
    thread_id: &str,
    session: &SessionRecord,
    request: &TurnRequest,
) -> Value {
    let effective_search_mode = request.override_search_mode.unwrap_or(session.search_mode);
    let prompt = request.prompt.trim();
    let mut input = vec![json!({"type":"text","text":prompt,"text_elements":[]})];
    for image in request.image_paths() {
        input.push(json!({"type":"localImage","path":sanitize_arg_path(&image)}));
    }
    json!({"threadId":thread_id,"input":input,"cwd":sanitize_arg_path(&session.cwd),"approvalPolicy":session.approval_policy,"sandboxPolicy":build_sandbox_policy(session),"model":session.model,"effort":session.reasoning_effort,"summary":Value::Null,"serviceTier":Value::Null,"outputSchema":Value::Null,"personality":Value::Null,"collaborationMode":Value::Null,"config":build_config_overrides(effective_search_mode)})
}

fn build_sandbox_policy(session: &SessionRecord) -> Value {
    match session.sandbox_mode.as_str() {
        "danger-full-access" => json!({"type":"dangerFullAccess"}),
        "workspace-write" => {
            json!({"type":"workspaceWrite","writableRoots":collect_session_roots(session),"networkAccess":true,"excludeTmpdirEnvVar":false,"excludeSlashTmp":false})
        }
        _ => json!({"type":"readOnly","networkAccess":true}),
    }
}
fn collect_session_roots(session: &SessionRecord) -> Vec<String> {
    let mut roots = BTreeSet::new();
    roots.insert(sanitize_arg_path(&session.cwd));
    for path in &session.add_dirs {
        roots.insert(sanitize_arg_path(path));
    }
    roots.into_iter().collect()
}
fn build_config_overrides(search_mode: SearchMode) -> Value {
    json!({"web_search":search_mode.as_codex_value()})
}

fn model_list_params(cursor: Option<&str>) -> Value {
    let mut params = serde_json::Map::from_iter([
        ("limit".to_string(), json!(100)),
        ("includeHidden".to_string(), json!(false)),
    ]);
    if let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) {
        params.insert("cursor".to_string(), json!(cursor));
    }
    Value::Object(params)
}

async fn run_simple_command(binary: &Path, args: &[&str]) -> Result<String> {
    let output = run_simple_command_capture(binary, args).await?;
    if !output.success {
        if output.message.is_empty() {
            bail!("codex command failed");
        }
        bail!("{}", output.message);
    }
    Ok(output.message)
}

async fn run_simple_command_capture(binary: &Path, args: &[&str]) -> Result<SimpleCommandOutput> {
    let spec = CommandSpec {
        program: binary.to_path_buf(),
        args: args.iter().map(|value| (*value).to_string()).collect(),
        current_dir: None,
    };
    let mut command = spawnable_command(&spec);
    let output = command.output().await.with_context(|| {
        format!(
            "failed to run codex command: {} {}",
            spec.program.display(),
            spec.args.join(" ")
        )
    })?;
    let stdout = strip_ansi_codes(&String::from_utf8_lossy(&output.stdout));
    let stderr = strip_ansi_codes(&String::from_utf8_lossy(&output.stderr));
    let message = if stdout.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        stdout.trim().to_string()
    };
    Ok(SimpleCommandOutput {
        success: output.status.success(),
        message,
    })
}

fn interpret_auth_status(output: SimpleCommandOutput) -> Result<CodexAuthStatus> {
    if output.success {
        return Ok(CodexAuthStatus {
            authenticated: output.message.starts_with("Logged in"),
            detail: output.message,
        });
    }
    if output.message.trim() == "Not logged in" {
        return Ok(CodexAuthStatus {
            authenticated: false,
            detail: output.message,
        });
    }
    if output.message.is_empty() {
        bail!("failed to read Codex login status");
    }
    bail!("{}", output.message)
}

fn has_api_key_auth(codex_home: &Path) -> Result<bool> {
    if std::env::var("OPENAI_API_KEY")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        return Ok(true);
    }

    let auth_path = codex_home.join("auth.json");
    if !auth_path.is_file() {
        return Ok(false);
    }

    let raw = fs::read_to_string(&auth_path)
        .with_context(|| format!("failed to read {}", auth_path.display()))?;
    let json: Value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", auth_path.display()))?;
    Ok(json
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false))
}

fn append_output_line(buffer: &mut String, line: &str) {
    if buffer.is_empty() {
        buffer.push_str(line);
    } else {
        buffer.push('\n');
        buffer.push_str(line);
    }
}

fn parse_device_auth_prompt(output: &str) -> Option<CodexDeviceAuthPrompt> {
    let clean = strip_ansi_codes(output);
    let verification_uri = clean
        .split_whitespace()
        .find(|token| token.starts_with("https://"))
        .map(ToOwned::to_owned)?;
    let lines = clean.lines().map(str::trim).collect::<Vec<_>>();
    let code_hint_index = lines.iter().position(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("one-time code") || lower.contains("device code")
    })?;
    let user_code = lines
        .iter()
        .skip(code_hint_index + 1)
        .flat_map(|line| line.split_whitespace())
        .map(trim_device_code_token)
        .find(|token| looks_like_device_code(token))
        .map(ToOwned::to_owned)?;
    Some(CodexDeviceAuthPrompt {
        verification_uri,
        user_code,
    })
}

fn looks_like_device_code(token: &str) -> bool {
    let mut parts = token.split('-');
    let left = parts.next().unwrap_or_default();
    let right = parts.next().unwrap_or_default();
    parts.next().is_none()
        && (4..=8).contains(&left.len())
        && (4..=8).contains(&right.len())
        && left
            .chars()
            .all(|value| value.is_ascii_uppercase() || value.is_ascii_digit())
        && right
            .chars()
            .all(|value| value.is_ascii_uppercase() || value.is_ascii_digit())
}

fn trim_device_code_token(token: &str) -> &str {
    token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
}

fn strip_ansi_codes(input: &str) -> String {
    let mut clean = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && matches!(chars.peek(), Some('[')) {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        clean.push(ch);
    }
    clean
}

fn build_command_approval_request(params: &Value) -> CodexApprovalRequest {
    let payload = approval_request_payload(params);
    let command = payload
        .get("command")
        .or_else(|| params.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("command");
    let cwd = payload
        .get("cwd")
        .or_else(|| params.get("cwd"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let reason = payload
        .get("reason")
        .or_else(|| params.get("reason"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut prompt = format!("Command approval needed.\n\n`{command}`");
    if let Some(cwd) = cwd {
        prompt.push_str(&format!("\n\nWorking directory: `{cwd}`"));
    }
    if let Some(reason) = reason {
        prompt.push_str(&format!("\n\nReason: {reason}"));
    }
    CodexApprovalRequest {
        kind: CodexApprovalKind::CommandExecution,
        prompt,
        options: approval_options(params),
    }
}

fn build_file_change_approval_request(params: &Value) -> CodexApprovalRequest {
    let payload = approval_request_payload(params);
    let grant_root = payload
        .get("grantRoot")
        .or_else(|| payload.get("grant_root"))
        .or_else(|| params.get("grantRoot"))
        .or_else(|| params.get("grant_root"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let reason = payload
        .get("reason")
        .or_else(|| params.get("reason"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut prompt = "File change approval needed.".to_string();
    if let Some(grant_root) = grant_root {
        prompt.push_str(&format!("\n\nWritable root: `{grant_root}`"));
    }
    if let Some(reason) = reason {
        prompt.push_str(&format!("\n\nReason: {reason}"));
    }
    CodexApprovalRequest {
        kind: CodexApprovalKind::FileChange,
        prompt,
        options: approval_options(params),
    }
}

fn approval_request_payload<'a>(params: &'a Value) -> &'a Value {
    params
        .get("request")
        .or_else(|| params.get("approvalRequest"))
        .or_else(|| params.get("approval_request"))
        .unwrap_or(params)
}

fn approval_options(params: &Value) -> Vec<CodexApprovalDecision> {
    let payload = approval_request_payload(params);
    let mut options = payload
        .get("availableDecisions")
        .or_else(|| payload.get("available_decisions"))
        .or_else(|| params.get("availableDecisions"))
        .or_else(|| params.get("available_decisions"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(approval_decision_from_value)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if options.is_empty() {
        options = vec![
            CodexApprovalDecision::Accept,
            CodexApprovalDecision::AcceptForSession,
            CodexApprovalDecision::Decline,
            CodexApprovalDecision::Cancel,
        ];
    }
    options
}

fn approval_decision_from_value(value: &Value) -> Option<CodexApprovalDecision> {
    let name = match value {
        Value::String(name) => Some(name.as_str()),
        Value::Object(map) => map
            .get("decision")
            .or_else(|| map.get("type"))
            .or_else(|| map.get("name"))
            .and_then(Value::as_str),
        _ => None,
    }?;
    match name {
        "accept" => Some(CodexApprovalDecision::Accept),
        "acceptForSession" | "accept_for_session" => Some(CodexApprovalDecision::AcceptForSession),
        "decline" => Some(CodexApprovalDecision::Decline),
        "cancel" => Some(CodexApprovalDecision::Cancel),
        _ => None,
    }
}

fn approval_decision_value(decision: CodexApprovalDecision) -> &'static str {
    match decision {
        CodexApprovalDecision::Accept => "accept",
        CodexApprovalDecision::AcceptForSession => "acceptForSession",
        CodexApprovalDecision::Decline => "decline",
        CodexApprovalDecision::Cancel => "cancel",
    }
}

fn outcome_to_approval_decision(outcome: CodexEventOutcome) -> CodexApprovalDecision {
    match outcome {
        CodexEventOutcome::Approval(decision) => decision,
        CodexEventOutcome::None => CodexApprovalDecision::Decline,
    }
}

fn merge_instruction_sections(
    session_prompt: Option<&str>,
    runtime_instructions: Option<&str>,
) -> Option<String> {
    let sections = [session_prompt, runtime_instructions]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|section| !section.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}
fn push_common_config_args(
    args: &mut Vec<String>,
    search_mode: SearchMode,
    approval_policy: &str,
    model: &Option<String>,
    reasoning_effort: Option<&str>,
    developer_instructions: Option<&str>,
) {
    args.push("-c".to_string());
    args.push(format!("approval_policy={approval_policy}"));
    args.push("-c".to_string());
    args.push(format!("web_search={}", search_mode.as_codex_value()));
    if let Some(reasoning_effort) = reasoning_effort {
        args.push("-c".to_string());
        args.push(format!("model_reasoning_effort={reasoning_effort}"));
    }
    if let Some(developer_instructions) = developer_instructions {
        args.push("-c".to_string());
        args.push(format!(
            "developer_instructions={}",
            toml::Value::String(developer_instructions.to_string())
        ));
    }
    if let Some(model) = model {
        args.push("-m".to_string());
        args.push(model.clone());
    }
}
impl AppServerProcess {
    async fn spawn(binary: &Path) -> Result<Self> {
        let spec = build_app_server_command(binary);
        let mut command = spawnable_command(&spec);
        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to spawn codex app-server: {} {}",
                spec.program.display(),
                spec.args.join(" ")
            )
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("codex app-server stdin pipe unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("codex app-server stdout pipe unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("codex app-server stderr pipe unavailable"))?;
        let stderr_buffer = Arc::new(Mutex::new(String::new()));
        let stderr_buffer_task = stderr_buffer.clone();
        let stderr_task = tokio::spawn(async move {
            let mut stderr_lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = stderr_lines.next_line().await {
                let mut buffer = stderr_buffer_task.lock().await;
                if !buffer.is_empty() {
                    buffer.push('\n');
                }
                buffer.push_str(&line);
                tracing::warn!("codex stderr: {line}");
            }
        });
        Ok(Self {
            child,
            stdin,
            stdout_lines: BufReader::new(stdout).lines(),
            stderr_buffer,
            stderr_task,
            next_id: 1,
        })
    }
    async fn initialize(&mut self) -> Result<()> {
        let request_id=self.send_request("initialize",json!({"clientInfo":{"name":"telecodex","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":false}})).await?;
        let _ = self.await_response(request_id).await?;
        self.send_notification("initialized").await?;
        Ok(())
    }
    async fn start_or_resume_thread(
        &mut self,
        session: &SessionRecord,
        request: &TurnRequest,
    ) -> Result<String> {
        let (method, params) = build_thread_request(session, request);
        let request_id = self.send_request(method, params).await?;
        let response = self.await_response(request_id).await?;
        response
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("app-server `{method}` response missing thread id"))
    }
    async fn send_request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_line(&json!({"method":method,"id":id,"params":params}))
            .await?;
        Ok(id)
    }
    async fn send_notification(&mut self, method: &str) -> Result<()> {
        self.write_line(&json!({"method":method})).await
    }
    async fn send_result(&mut self, id: u64, result: Value) -> Result<()> {
        self.write_line(&json!({"id":id,"result":result})).await
    }
    async fn await_response(&mut self, expected_id: u64) -> Result<Value> {
        loop {
            let Some(message) = self.next_message().await? else {
                bail!("codex app-server closed before responding to request {expected_id}");
            };
            match message {
                RpcMessage::Response { id, result, error } => {
                    if id != expected_id {
                        continue;
                    }
                    if let Some(error) = error {
                        bail!("{}", format_rpc_error(&error));
                    }
                    return Ok(result.unwrap_or(Value::Null));
                }
                RpcMessage::Notification { .. } => {}
                RpcMessage::ServerRequest { id, method, .. } => {
                    bail!(
                        "unexpected server request `{method}` before response {expected_id} (request id {id})"
                    );
                }
            }
        }
    }
    async fn next_message(&mut self) -> Result<Option<RpcMessage>> {
        let Some(line) = self
            .stdout_lines
            .next_line()
            .await
            .context("reading app-server stdout failed")?
        else {
            return Ok(None);
        };
        parse_rpc_message(&line)
    }
    async fn write_line(&mut self, value: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .context("writing app-server request failed")?;
        self.stdin
            .flush()
            .await
            .context("flushing app-server stdin failed")
    }
    async fn shutdown(mut self) -> Result<String> {
        terminate_child(&mut self.child).await;
        if tokio::time::timeout(Duration::from_secs(2), &mut self.stderr_task)
            .await
            .is_err()
        {
            tracing::debug!("codex app-server stderr task did not exit promptly; aborting");
            self.stderr_task.abort();
        }
        Ok(self.stderr_buffer.lock().await.trim().to_string())
    }
}

fn parse_rpc_message(line: &str) -> Result<Option<RpcMessage>> {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => {
            tracing::debug!("ignoring non-json app-server stdout line: {line}");
            return Ok(None);
        }
    };
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);
    let id = value.get("id").and_then(Value::as_u64);
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    let result = value.get("result").cloned();
    let error = value
        .get("error")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    Ok(Some(match (method, id, result, error) {
        (Some(method), Some(id), _, _) => RpcMessage::ServerRequest { id, method, params },
        (Some(method), None, _, _) => RpcMessage::Notification { method, params },
        (None, Some(id), result, error) => RpcMessage::Response { id, result, error },
        _ => return Ok(None),
    }))
}
fn format_rpc_error(error: &RpcError) -> String {
    let mut parts = vec![error.message.clone()];
    if let Some(code) = error.code {
        parts.push(format!("code {code}"));
    }
    if let Some(data) = &error.data {
        parts.push(data.to_string());
    }
    parts.join(" | ")
}
fn spawnable_command(spec: &CommandSpec) -> Command {
    #[cfg(windows)]
    {
        let ext = spec
            .program
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("cmd" | "bat")) {
            let mut command = Command::new("cmd.exe");
            command.arg("/C").arg(&spec.program).args(&spec.args);
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(current_dir) = &spec.current_dir {
                command.current_dir(current_dir);
            }
            return command;
        }
    }
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(current_dir) = &spec.current_dir {
        command.current_dir(current_dir);
    }
    command
}
fn sanitize_arg_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        let raw = path.as_os_str().to_string_lossy();
        if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = raw.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
    }
    path.display().to_string()
}
async fn terminate_child(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            if let Err(error) = child.kill().await {
                tracing::warn!("failed to kill codex child: {error}");
            }
        }
        Err(error) => {
            tracing::warn!("failed to inspect codex child status: {error}");
        }
    }
    let _ = child.wait().await;
}
fn parse_exec_event(line: &str) -> Result<Option<CodexEvent>> {
    let envelope: ExecEnvelope = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => {
            tracing::debug!("ignoring non-json codex stdout line: {line}");
            return Ok(None);
        }
    };
    match envelope.kind.as_str() {
        "thread.started" => Ok(envelope.thread_id.map(CodexEvent::ThreadStarted)),
        "item.started" => {
            if let Some(item) = envelope.item {
                if item.item_type.as_deref() == Some("command_execution") {
                    let command = item.command.unwrap_or_else(|| "command".to_string());
                    return Ok(Some(CodexEvent::Progress(format!("Running `{command}`"))));
                }
            }
            Ok(None)
        }
        "item.completed" => {
            if let Some(item) = envelope.item {
                match item.item_type.as_deref() {
                    Some("agent_message") => Ok(Some(CodexEvent::AssistantText(
                        item.text.unwrap_or_default(),
                    ))),
                    Some("command_execution") => {
                        let status = item.status.unwrap_or_else(|| "completed".to_string());
                        let command = item.command.unwrap_or_else(|| "command".to_string());
                        let output = item.aggregated_output.unwrap_or_default();
                        let summary = if output.trim().is_empty() {
                            format!("{command} {status}")
                        } else {
                            format!("{command} {status}\n{}", output.trim())
                        };
                        Ok(Some(CodexEvent::Progress(summary)))
                    }
                    _ => Ok(None),
                }
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}
#[derive(Debug, Deserialize)]
struct ExecEnvelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    item: Option<ExecItem>,
}
#[derive(Debug, Deserialize)]
struct ExecItem {
    #[serde(rename = "type")]
    item_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    aggregated_output: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_workspace() -> PathBuf {
        std::env::temp_dir()
            .join("telecodex-tests")
            .join("workspace")
    }

    fn sample_add_dir() -> PathBuf {
        std::env::temp_dir().join("telecodex-tests").join("shared")
    }

    fn session_with_sandbox(mode: &str) -> SessionRecord {
        SessionRecord {
            id: 1,
            key: crate::models::SessionKey {
                chat_id: 1,
                thread_id: 2,
            },
            session_title: None,
            codex_thread_id: None,
            force_fresh_thread: false,
            updated_at: "2026-03-13T10:00:00Z".to_string(),
            cwd: sample_workspace(),
            model: Some("gpt-5.4".to_string()),
            reasoning_effort: None,
            session_prompt: None,
            sandbox_mode: mode.to_string(),
            approval_policy: "never".to_string(),
            search_mode: SearchMode::Disabled,
            add_dirs: vec![sample_add_dir()],
            busy: false,
        }
    }

    fn review_request() -> TurnRequest {
        TurnRequest {
            session_key: crate::models::SessionKey {
                chat_id: 1,
                thread_id: 2,
            },
            from_user_id: 42,
            prompt: "look for bugs".to_string(),
            runtime_instructions: None,
            attachments: vec![],
            review_mode: Some(crate::models::ReviewRequest {
                base: Some("main".to_string()),
                commit: None,
                uncommitted: false,
                title: Some("Review".to_string()),
                prompt: Some("focus on regressions".to_string()),
            }),
            override_search_mode: None,
        }
    }

    #[test]
    fn workspace_write_keeps_network_enabled() {
        let session = session_with_sandbox("workspace-write");
        let policy = build_sandbox_policy(&session);
        assert_eq!(
            policy.get("type").and_then(Value::as_str),
            Some("workspaceWrite")
        );
        assert_eq!(
            policy.get("networkAccess").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn read_only_keeps_network_enabled() {
        let session = session_with_sandbox("read-only");
        let policy = build_sandbox_policy(&session);
        assert_eq!(policy.get("type").and_then(Value::as_str), Some("readOnly"));
        assert_eq!(
            policy.get("networkAccess").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn read_only_omits_legacy_restricted_access() {
        let session = session_with_sandbox("read-only");
        let policy = build_sandbox_policy(&session);
        assert!(policy.get("access").is_none());
    }

    #[test]
    fn workspace_write_omits_legacy_restricted_read_only_access() {
        let session = session_with_sandbox("workspace-write");
        let policy = build_sandbox_policy(&session);
        assert!(policy.get("readOnlyAccess").is_none());
    }

    #[test]
    fn builds_model_list_params_without_cursor() {
        assert_eq!(
            model_list_params(None),
            json!({
                "limit": 100,
                "includeHidden": false
            })
        );
    }

    #[test]
    fn builds_model_list_params_with_cursor() {
        assert_eq!(
            model_list_params(Some("abc123")),
            json!({
                "limit": 100,
                "includeHidden": false,
                "cursor": "abc123"
            })
        );
    }

    #[test]
    fn build_review_command_uses_session_cwd() {
        let session = session_with_sandbox("workspace-write");
        let request = review_request();
        let spec = build_review_command(
            Path::new("codex"),
            &session,
            &request,
            request.review_mode.as_ref().unwrap(),
        );

        assert_eq!(spec.current_dir.as_deref(), Some(session.cwd.as_path()));
        assert!(spec.args.contains(&"review".to_string()));
    }

    #[test]
    fn parses_model_list_page() {
        let page: ModelListPage = serde_json::from_value(json!({
            "data": [
                {
                    "id": "gpt-5.4",
                    "displayName": "gpt-5.4",
                    "description": "Latest frontier agentic coding model.",
                    "isDefault": true
                }
            ],
            "nextCursor": null
        }))
        .expect("parse model page");

        assert_eq!(page.data.len(), 1);
        assert_eq!(page.data[0].id, "gpt-5.4");
        assert_eq!(page.data[0].display_name.as_deref(), Some("gpt-5.4"));
        assert_eq!(
            page.data[0].description.as_deref(),
            Some("Latest frontier agentic coding model.")
        );
        assert!(page.data[0].is_default);
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn strips_ansi_sequences_from_codex_output() {
        let clean = strip_ansi_codes("\u{1b}[94mhttps://auth.openai.com/codex/device\u{1b}[0m");
        assert_eq!(clean, "https://auth.openai.com/codex/device");
    }

    #[test]
    fn parses_device_auth_prompt_from_headless_login_output() {
        let prompt = parse_device_auth_prompt(
            "OpenAI's command-line coding agent\n\n1. Open this link in your browser and sign in to your account\n   \u{1b}[94mhttps://auth.openai.com/codex/device\u{1b}[0m\n\n2. Enter this one-time code \u{1b}[90m(expires in 15 minutes)\u{1b}[0m\n   \u{1b}[94mXD4O-JA94K\u{1b}[0m",
        )
        .expect("device auth prompt");

        assert_eq!(
            prompt.verification_uri,
            "https://auth.openai.com/codex/device"
        );
        assert_eq!(prompt.user_code, "XD4O-JA94K");
    }

    #[test]
    fn interprets_not_logged_in_as_valid_auth_state() {
        let status = interpret_auth_status(SimpleCommandOutput {
            success: false,
            message: "Not logged in".to_string(),
        })
        .expect("auth status");

        assert!(!status.authenticated);
        assert_eq!(status.detail, "Not logged in");
    }

    #[tokio::test]
    async fn completes_turn_when_legacy_thread_goes_idle_after_agent_message() {
        let mut summary = RunSummary {
            codex_thread_id: None,
            assistant_text: String::new(),
            stderr_text: String::new(),
        };
        let mut active_turn_id = Some("turn-1".to_string());
        let mut turn_error = None;
        let mut turn_completed = false;
        let mut assistant_message_completed = false;
        let mut on_event = |_event| async { Ok(CodexEventOutcome::None) };

        handle_notification(
            "item/completed",
            &json!({
                "item": {
                    "type": "agentMessage",
                    "text": "Вижу."
                }
            }),
            &mut summary,
            &mut active_turn_id,
            &mut turn_error,
            &mut turn_completed,
            &mut assistant_message_completed,
            &mut on_event,
        )
        .await
        .expect("handle agent message");

        assert_eq!(summary.assistant_text, "Вижу.");
        assert!(assistant_message_completed);
        assert!(!turn_completed);

        handle_notification(
            "thread/status/changed",
            &json!({
                "status": {
                    "type": "idle"
                }
            }),
            &mut summary,
            &mut active_turn_id,
            &mut turn_error,
            &mut turn_completed,
            &mut assistant_message_completed,
            &mut on_event,
        )
        .await
        .expect("handle idle status");

        assert!(turn_completed);
        assert!(active_turn_id.is_none());
        assert!(turn_error.is_none());
    }

    #[tokio::test]
    async fn ignores_idle_without_completed_agent_message() {
        let mut summary = RunSummary {
            codex_thread_id: None,
            assistant_text: String::new(),
            stderr_text: String::new(),
        };
        let mut active_turn_id = Some("turn-1".to_string());
        let mut turn_error = None;
        let mut turn_completed = false;
        let mut assistant_message_completed = false;
        let mut on_event = |_event| async { Ok(CodexEventOutcome::None) };

        handle_notification(
            "thread/status/changed",
            &json!({
                "status": {
                    "type": "idle"
                }
            }),
            &mut summary,
            &mut active_turn_id,
            &mut turn_error,
            &mut turn_completed,
            &mut assistant_message_completed,
            &mut on_event,
        )
        .await
        .expect("handle idle status");

        assert!(!turn_completed);
        assert_eq!(active_turn_id.as_deref(), Some("turn-1"));
    }

    #[tokio::test]
    async fn completes_turn_on_final_answer_item_completed() {
        let mut summary = RunSummary {
            codex_thread_id: None,
            assistant_text: String::new(),
            stderr_text: String::new(),
        };
        let mut active_turn_id = Some("turn-1".to_string());
        let mut turn_error = None;
        let mut turn_completed = false;
        let mut assistant_message_completed = false;
        let mut on_event = |_event| async { Ok(CodexEventOutcome::None) };

        handle_notification(
            "item/completed",
            &json!({
                "item": {
                    "type": "agentMessage",
                    "text": "На связи.",
                    "phase": "final_answer"
                }
            }),
            &mut summary,
            &mut active_turn_id,
            &mut turn_error,
            &mut turn_completed,
            &mut assistant_message_completed,
            &mut on_event,
        )
        .await
        .expect("handle final answer");

        assert_eq!(summary.assistant_text, "На связи.");
        assert!(assistant_message_completed);
        assert!(turn_completed);
        assert!(active_turn_id.is_none());
        assert!(turn_error.is_none());
    }

    #[tokio::test]
    async fn commentary_agent_message_does_not_complete_turn() {
        let mut summary = RunSummary {
            codex_thread_id: None,
            assistant_text: String::new(),
            stderr_text: String::new(),
        };
        let mut active_turn_id = Some("turn-1".to_string());
        let mut turn_error = None;
        let mut turn_completed = false;
        let mut assistant_message_completed = false;
        let mut on_event = |_event| async { Ok(CodexEventOutcome::None) };

        handle_notification(
            "item/completed",
            &json!({
                "item": {
                    "type": "agentMessage",
                    "text": "Смотрю логи.",
                    "phase": "commentary"
                }
            }),
            &mut summary,
            &mut active_turn_id,
            &mut turn_error,
            &mut turn_completed,
            &mut assistant_message_completed,
            &mut on_event,
        )
        .await
        .expect("handle commentary message");

        assert_eq!(summary.assistant_text, "Смотрю логи.");
        assert!(assistant_message_completed);
        assert!(!turn_completed);
        assert_eq!(active_turn_id.as_deref(), Some("turn-1"));
    }
}
