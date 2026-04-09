use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;
use uuid::Uuid;

const SEEDED_ENV_UPDATED_AT: &str = "~~~~~~~~seeded";

#[derive(Debug, Clone)]
pub struct CodexThreadSummary {
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub updated_at: String,
    pub source: CodexHistorySource,
}

#[derive(Debug, Clone)]
pub struct CodexEnvironmentSummary {
    pub cwd: PathBuf,
    pub name: String,
    pub latest_thread_id: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexHistorySource {
    Desktop,
    Cli,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexHistoryEntry {
    pub role: String,
    pub text: String,
    pub timestamp: String,
}

#[derive(Debug, Deserialize)]
struct SessionIndexEntry {
    id: String,
    #[serde(default)]
    thread_name: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RolloutEnvelope {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    payload: Option<RolloutSessionMeta>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<Vec<RolloutContentBlock>>,
}

#[derive(Debug, Deserialize)]
struct RolloutContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RolloutSessionMeta {
    id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    originator: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LegacyRolloutMeta {
    id: String,
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EnvironmentToml {
    #[serde(default)]
    name: Option<String>,
}

pub fn latest_thread_for_cwd(codex_home: &Path, cwd: &Path) -> Result<Option<CodexThreadSummary>> {
    Ok(list_threads_for_cwd(codex_home, cwd, 1)?.into_iter().next())
}

pub fn environment_identity_for_cwd(cwd: &Path) -> PathBuf {
    git_environment_root(cwd).unwrap_or_else(|| normalize_path(cwd.to_path_buf()))
}

pub fn find_thread_by_id(codex_home: &Path, thread_id: &str) -> Result<Option<CodexThreadSummary>> {
    Ok(list_all_threads(codex_home)?
        .into_iter()
        .find(|thread| thread.id == thread_id))
}

pub fn find_thread_by_prefix(
    codex_home: &Path,
    cwd: &Path,
    prefix: &str,
) -> Result<Option<CodexThreadSummary>> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Ok(None);
    }
    let threads = list_threads_for_cwd(codex_home, cwd, 200)?;
    let mut matches = threads
        .into_iter()
        .filter(|thread| thread_matches_selector(thread, prefix))
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        Ok(matches.pop())
    } else {
        Ok(None)
    }
}

pub fn read_thread_history(
    codex_home: &Path,
    thread_id: &str,
    limit: usize,
) -> Result<Vec<CodexHistoryEntry>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let Some(path) = find_rollout_file_for_thread(codex_home, thread_id)? else {
        return Ok(Vec::new());
    };
    read_rollout_history(&path, limit)
}

pub fn list_threads_for_cwd(
    codex_home: &Path,
    cwd: &Path,
    limit: usize,
) -> Result<Vec<CodexThreadSummary>> {
    let target_cwd = environment_identity_for_cwd(cwd);
    let mut summaries = list_all_threads(codex_home)?;
    summaries.retain(|summary| environment_identity_for_cwd(&summary.cwd) == target_cwd);
    if limit > 0 && summaries.len() > limit {
        summaries.truncate(limit);
    }
    Ok(summaries)
}

pub fn list_environments_for_sources(
    codex_home: &Path,
    limit: usize,
    import_desktop: bool,
    import_cli: bool,
    seed_workspaces: &[PathBuf],
) -> Result<Vec<CodexEnvironmentSummary>> {
    list_environments_filtered(codex_home, limit, seed_workspaces, |source| match source {
        CodexHistorySource::Desktop => import_desktop,
        CodexHistorySource::Cli => import_cli,
        CodexHistorySource::Unknown => import_desktop || import_cli,
    })
}

fn list_environments_filtered<F>(
    codex_home: &Path,
    limit: usize,
    seed_workspaces: &[PathBuf],
    mut allow_source: F,
) -> Result<Vec<CodexEnvironmentSummary>>
where
    F: FnMut(CodexHistorySource) -> bool,
{
    let mut environments: HashMap<PathBuf, CodexEnvironmentSummary> = HashMap::new();
    for thread in list_all_threads(codex_home)? {
        if !allow_source(thread.source) {
            continue;
        }
        let environment_cwd = environment_identity_for_cwd(&thread.cwd);
        let candidate = CodexEnvironmentSummary {
            cwd: environment_cwd.clone(),
            name: environment_name_for_cwd(&environment_cwd, &thread.title),
            latest_thread_id: Some(thread.id.clone()),
            updated_at: thread.updated_at.clone(),
        };
        match environments.get(&environment_cwd) {
            Some(existing) if existing.updated_at >= candidate.updated_at => {}
            _ => {
                environments.insert(environment_cwd, candidate);
            }
        }
    }
    for workspace in seed_workspaces {
        let environment_cwd = environment_identity_for_cwd(workspace);
        environments
            .entry(environment_cwd.clone())
            .or_insert_with(|| CodexEnvironmentSummary {
                cwd: environment_cwd.clone(),
                name: environment_name_for_cwd(
                    &environment_cwd,
                    workspace
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("Workspace"),
                ),
                latest_thread_id: None,
                updated_at: SEEDED_ENV_UPDATED_AT.to_string(),
            });
    }
    let mut items = environments.into_values().collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.cwd.cmp(&right.cwd))
    });
    if limit > 0 && items.len() > limit {
        items.truncate(limit);
    }
    Ok(items)
}

pub fn environment_selector_key(environment: &CodexEnvironmentSummary) -> String {
    format!(
        "cwd:{}",
        Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            environment.cwd.to_string_lossy().as_bytes()
        )
    )
}

fn list_all_threads(codex_home: &Path) -> Result<Vec<CodexThreadSummary>> {
    let titles = read_session_index(&codex_home.join("session_index.jsonl"))?;
    let mut threads = HashMap::new();
    for summary in scan_rollout_sessions(&codex_home.join("archived_sessions"))? {
        threads.insert(summary.id.clone(), summary);
    }
    for summary in scan_rollout_sessions(&codex_home.join("sessions"))? {
        threads.insert(summary.id.clone(), summary);
    }
    let mut summaries = threads.into_values().collect::<Vec<_>>();
    for summary in &mut summaries {
        if let Some(index) = titles.get(&summary.id) {
            if let Some(title) = index.thread_name.as_deref().map(str::trim) {
                if !title.is_empty() {
                    summary.title = title.to_string();
                }
            }
            if let Some(updated_at) = index.updated_at.as_deref() {
                summary.updated_at = updated_at.to_string();
            }
        }
    }
    summaries.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    Ok(summaries)
}

fn read_session_index(path: &Path) -> Result<HashMap<String, SessionIndexEntry>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to open {}", path.display()));
        }
    };
    let mut entries: HashMap<String, SessionIndexEntry> = HashMap::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionIndexEntry = serde_json::from_str(&line)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        match entries.get(&entry.id) {
            Some(existing)
                if existing.updated_at.as_deref().unwrap_or_default()
                    >= entry.updated_at.as_deref().unwrap_or_default() => {}
            _ => {
                entries.insert(entry.id.clone(), entry);
            }
        }
    }
    Ok(entries)
}

fn scan_rollout_sessions(root: &Path) -> Result<Vec<CodexThreadSummary>> {
    let mut files = Vec::new();
    collect_rollout_files(root, &mut files)?;
    let mut threads = HashMap::new();
    for file in files {
        if let Some(summary) = read_rollout_session_meta(&file)? {
            threads.insert(summary.id.clone(), summary);
        }
    }
    Ok(threads.into_values().collect())
}

fn find_rollout_file_for_thread(codex_home: &Path, thread_id: &str) -> Result<Option<PathBuf>> {
    for root in [
        codex_home.join("sessions"),
        codex_home.join("archived_sessions"),
    ] {
        if let Some(path) = find_rollout_file_for_thread_in_root(&root, thread_id)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn find_rollout_file_for_thread_in_root(root: &Path, thread_id: &str) -> Result<Option<PathBuf>> {
    let mut files = Vec::new();
    collect_rollout_files(root, &mut files)?;
    files.sort();
    files.reverse();
    for file in files {
        if file
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.contains(thread_id))
        {
            return Ok(Some(file));
        }
        if read_rollout_session_meta(&file)?
            .as_ref()
            .is_some_and(|summary| summary.id == thread_id)
        {
            return Ok(Some(file));
        }
    }
    Ok(None)
}

fn collect_rollout_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", root.display()));
        }
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_rollout_files(&path, files)?;
        } else if path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.starts_with("rollout-") && value.ends_with(".jsonl"))
        {
            files.push(path);
        }
    }
    Ok(())
}

fn read_rollout_session_meta(path: &Path) -> Result<Option<CodexThreadSummary>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut legacy_meta: Option<LegacyRolloutMeta> = None;
    let mut summary_meta: Option<(String, String, Option<String>, CodexHistorySource)> = None;
    let mut preview_title: Option<String> = None;
    for line in BufReader::new(file).lines().take(120) {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if preview_title.is_none() {
            let value: serde_json::Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            preview_title = thread_preview_from_rollout_value(&value);
            if let Some((id, cwd, timestamp, source)) = summary_meta.as_ref() {
                if preview_title.is_some() {
                    return Ok(Some(build_thread_summary(
                        id.clone(),
                        cwd.clone(),
                        timestamp.clone(),
                        *source,
                        preview_title,
                    )));
                }
            }
        }
        if let Ok(envelope) = serde_json::from_str::<RolloutEnvelope>(&line) {
            if envelope.kind.as_deref() == Some("session_meta") {
                let Some(meta) = envelope.payload else {
                    return Ok(None);
                };
                if let Some(cwd) = meta.cwd {
                    summary_meta = Some((
                        meta.id,
                        cwd,
                        meta.timestamp,
                        classify_history_source(meta.source.as_deref(), meta.originator.as_deref()),
                    ));
                    if let Some((id, cwd, timestamp, source)) = summary_meta.as_ref() {
                        if preview_title.is_some() {
                            return Ok(Some(build_thread_summary(
                                id.clone(),
                                cwd.clone(),
                                timestamp.clone(),
                                *source,
                                preview_title,
                            )));
                        }
                    }
                    continue;
                }
                legacy_meta = Some(LegacyRolloutMeta {
                    id: meta.id,
                    timestamp: meta.timestamp,
                });
                continue;
            }
            if let Some(meta) = legacy_meta.as_ref() {
                if let Some(cwd) = extract_cwd_from_legacy_envelope(&envelope) {
                    summary_meta = Some((
                        meta.id.clone(),
                        cwd,
                        meta.timestamp.clone(),
                        CodexHistorySource::Unknown,
                    ));
                    if let Some((id, cwd, timestamp, source)) = summary_meta.as_ref() {
                        if preview_title.is_some() {
                            return Ok(Some(build_thread_summary(
                                id.clone(),
                                cwd.clone(),
                                timestamp.clone(),
                                *source,
                                preview_title,
                            )));
                        }
                    }
                }
            }
        }
        if legacy_meta.is_none() {
            legacy_meta = serde_json::from_str::<LegacyRolloutMeta>(&line).ok();
        }
    }
    Ok(summary_meta.map(|(id, cwd, timestamp, source)| {
        build_thread_summary(id, cwd, timestamp, source, preview_title)
    }))
}

fn read_rollout_history(path: &Path, limit: usize) -> Result<Vec<CodexHistoryEntry>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(kind) = value.get("type").and_then(|kind| kind.as_str()) else {
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let Some(payload) = value.get("payload") else {
            continue;
        };
        let candidate = match kind {
            "event_msg" => history_entry_from_event_payload(payload, timestamp),
            "response_item" => history_entry_from_response_payload(payload, timestamp),
            _ => None,
        };
        let Some(entry) = candidate else {
            continue;
        };
        if entries.last().is_some_and(|last: &CodexHistoryEntry| {
            last.role == entry.role && last.text == entry.text
        }) {
            continue;
        }
        entries.push(entry);
    }
    if entries.len() > limit {
        entries = entries.split_off(entries.len() - limit);
    }
    Ok(entries)
}

fn history_entry_from_event_payload(
    payload: &serde_json::Value,
    timestamp: String,
) -> Option<CodexHistoryEntry> {
    let payload_type = payload.get("type")?.as_str()?;
    match payload_type {
        "user_message" => build_history_entry("user", payload.get("message")?.as_str()?, timestamp),
        "agent_message"
            if payload.get("phase").and_then(|value| value.as_str()) == Some("final_answer") =>
        {
            build_history_entry("assistant", payload.get("message")?.as_str()?, timestamp)
        }
        _ => None,
    }
}

fn history_entry_from_response_payload(
    payload: &serde_json::Value,
    timestamp: String,
) -> Option<CodexHistoryEntry> {
    if payload.get("type").and_then(|value| value.as_str()) != Some("message") {
        return None;
    }
    let role = match payload.get("role").and_then(|value| value.as_str()) {
        Some("user") => "user",
        Some("assistant") => "assistant",
        _ => return None,
    };
    if role == "assistant" {
        match payload.get("phase").and_then(|value| value.as_str()) {
            Some("final_answer") | None => {}
            _ => return None,
        }
    }
    let content = payload.get("content")?.as_array()?;
    let text = content
        .iter()
        .filter_map(|item| {
            let kind = item.get("type").and_then(|value| value.as_str());
            if !response_content_item_matches_role(role, kind) {
                return None;
            }
            item.get("text").and_then(|value| value.as_str())
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    build_history_entry(role, &text, timestamp)
}

fn response_content_item_matches_role(role: &str, kind: Option<&str>) -> bool {
    match role {
        "user" => matches!(kind, Some("input_text") | None),
        "assistant" => matches!(kind, Some("output_text") | None),
        _ => false,
    }
}

fn build_history_entry(role: &str, text: &str, timestamp: String) -> Option<CodexHistoryEntry> {
    let text = normalize_history_text(role, text)?;
    Some(CodexHistoryEntry {
        role: role.to_string(),
        text,
        timestamp,
    })
}

fn normalize_history_text(role: &str, text: &str) -> Option<String> {
    let mut text = text.replace("\r\n", "\n");
    if role == "user" {
        if let Some((_, tail)) = text.rsplit_once("\n\nUser request:\n") {
            text = tail.to_string();
        }
        if let Some((head, _)) = text.split_once("\n\nFollow these instructions for this turn:") {
            text = head.to_string();
        }
        if text.contains("# AGENTS.md instructions") && !text.contains("\n\nUser request:\n") {
            return None;
        }
    }
    let text = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return None;
    }
    Some(text)
}

fn thread_matches_selector(thread: &CodexThreadSummary, selector: &str) -> bool {
    let selector = selector.trim().trim_matches('`');
    if selector.is_empty() {
        return false;
    }
    let selector_lower = selector.to_ascii_lowercase();
    let thread_id_lower = thread.id.to_ascii_lowercase();
    if thread_id_lower == selector_lower || thread_id_lower.starts_with(&selector_lower) {
        return true;
    }
    let normalized_selector = selector_lower.replace('-', "");
    let normalized_thread = thread_id_lower.replace('-', "");
    if normalized_thread == normalized_selector
        || normalized_thread.starts_with(&normalized_selector)
    {
        return true;
    }
    match split_thread_selector(selector) {
        Some((start, end)) => {
            normalized_thread.starts_with(&start) && normalized_thread.ends_with(&end)
        }
        None => false,
    }
}

fn split_thread_selector(selector: &str) -> Option<(String, String)> {
    if let Some((start, end)) = selector.split_once('…') {
        let start = start
            .trim()
            .trim_matches('`')
            .replace('-', "")
            .to_ascii_lowercase();
        let end = end
            .trim()
            .trim_matches('`')
            .replace('-', "")
            .to_ascii_lowercase();
        if !start.is_empty() && !end.is_empty() {
            return Some((start, end));
        }
    }
    if let Some((start, end)) = selector.split_once("...") {
        let start = start
            .trim()
            .trim_matches('`')
            .replace('-', "")
            .to_ascii_lowercase();
        let end = end
            .trim()
            .trim_matches('`')
            .replace('-', "")
            .to_ascii_lowercase();
        if !start.is_empty() && !end.is_empty() {
            return Some((start, end));
        }
    }
    None
}

fn extract_cwd_from_legacy_envelope(envelope: &RolloutEnvelope) -> Option<String> {
    if envelope.role.as_deref() != Some("user") {
        return None;
    }
    for block in envelope.content.as_deref().unwrap_or(&[]) {
        if block.kind != "input_text" {
            continue;
        }
        let text = block.text.as_deref()?;
        if let Some(cwd) = extract_cwd_from_environment_context(text) {
            return Some(cwd);
        }
    }
    None
}

fn extract_cwd_from_environment_context(text: &str) -> Option<String> {
    let marker = "Current working directory:";
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(marker) {
            let cwd = rest.trim();
            if !cwd.is_empty() {
                return Some(cwd.to_string());
            }
        }
    }
    None
}

fn thread_preview_from_rollout_value(value: &serde_json::Value) -> Option<String> {
    match value.get("type").and_then(|entry| entry.as_str()) {
        Some("event_msg") => value
            .get("payload")
            .and_then(|payload| {
                if payload.get("type").and_then(|entry| entry.as_str()) != Some("user_message") {
                    return None;
                }
                payload.get("message").and_then(|message| message.as_str())
            })
            .and_then(normalize_thread_preview),
        Some("response_item") => value.get("payload").and_then(|payload| {
            if payload.get("type").and_then(|entry| entry.as_str()) != Some("message") {
                return None;
            }
            if payload.get("role").and_then(|entry| entry.as_str()) != Some("user") {
                return None;
            }
            let content = payload.get("content")?.as_array()?;
            let text = content
                .iter()
                .filter_map(|item| item.get("text").and_then(|entry| entry.as_str()))
                .collect::<Vec<_>>()
                .join("\n\n");
            normalize_thread_preview(&text)
        }),
        _ => None,
    }
}

fn normalize_thread_preview(text: &str) -> Option<String> {
    let normalized = normalize_history_text("user", text)?;
    let first_line = normalized.lines().find(|line| !line.trim().is_empty())?;
    let collapsed = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    const LIMIT: usize = 96;
    if collapsed.chars().count() <= LIMIT {
        Some(collapsed)
    } else {
        let truncated = collapsed.chars().take(LIMIT - 1).collect::<String>();
        Some(format!("{truncated}..."))
    }
}

fn build_thread_summary(
    id: String,
    cwd: String,
    timestamp: Option<String>,
    source: CodexHistorySource,
    preview_title: Option<String>,
) -> CodexThreadSummary {
    let fallback_title = id
        .split('-')
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| id.clone());
    CodexThreadSummary {
        title: preview_title.unwrap_or(fallback_title),
        id,
        cwd: normalize_path(PathBuf::from(cwd)),
        updated_at: timestamp.unwrap_or_default(),
        source,
    }
}

fn classify_history_source(source: Option<&str>, originator: Option<&str>) -> CodexHistorySource {
    let source = source
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let originator = originator
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if source == "vscode" || originator.contains("desktop") {
        CodexHistorySource::Desktop
    } else if source == "exec" || originator.contains("exec") {
        CodexHistorySource::Cli
    } else {
        CodexHistorySource::Unknown
    }
}

fn environment_name_for_cwd(cwd: &Path, fallback: &str) -> String {
    read_environment_name(cwd).unwrap_or_else(|| {
        cwd.file_name()
            .and_then(|value| value.to_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| fallback.to_string())
    })
}

fn read_environment_name(cwd: &Path) -> Option<String> {
    let path = cwd
        .join(".codex")
        .join("environments")
        .join("environment.toml");
    let raw = fs::read_to_string(path).ok()?;
    let parsed: EnvironmentToml = toml::from_str(&raw).ok()?;
    parsed
        .name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn git_environment_root(cwd: &Path) -> Option<PathBuf> {
    let git_path = cwd.join(".git");
    if git_path.is_dir() {
        return Some(normalize_path(cwd.to_path_buf()));
    }
    let raw = fs::read_to_string(&git_path).ok()?;
    let gitdir = raw.trim().strip_prefix("gitdir:")?.trim();
    let gitdir_path = git_path.parent()?.join(gitdir);
    let gitdir_path = normalize_path(fs::canonicalize(gitdir_path).ok()?);
    let commondir = fs::read_to_string(gitdir_path.join("commondir")).ok()?;
    let common_dir = gitdir_path.join(commondir.trim());
    let common_dir = normalize_path(fs::canonicalize(common_dir).ok()?);
    Some(normalize_path(common_dir.parent()?.to_path_buf()))
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
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use super::*;

    fn escaped_json_path(path: &Path) -> String {
        path.display().to_string().replace('\\', "\\\\")
    }

    fn workspace_path(root: &Path, name: &str) -> PathBuf {
        root.join("workspaces").join(name)
    }

    fn archived_sessions_dir(root: &Path) -> PathBuf {
        root.join("archived_sessions")
            .join("2026")
            .join("03")
            .join("13")
    }

    #[test]
    fn lists_threads_for_matching_cwd() {
        let dir = tempdir().unwrap();
        let cwd = workspace_path(dir.path(), "telecodex");
        let sessions_dir = dir
            .path()
            .join("sessions")
            .join("2026")
            .join("03")
            .join("13");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            dir.path().join("session_index.jsonl"),
            "{\"id\":\"abc123\",\"thread_name\":\"From index\",\"updated_at\":\"2026-03-13T10:00:00Z\"}\n",
        )
        .unwrap();
        fs::write(
            sessions_dir.join("rollout-1.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"abc123\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"exec\"}}}}\n",
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();

        let threads = list_threads_for_cwd(dir.path(), &cwd, 10).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "abc123");
        assert_eq!(threads[0].title, "From index");
        assert_eq!(threads[0].source, CodexHistorySource::Cli);
    }

    #[test]
    fn filters_environments_by_selected_sources_before_grouping() {
        let dir = tempdir().unwrap();
        let cwd = workspace_path(dir.path(), "telecodex");
        let sessions_dir = dir
            .path()
            .join("sessions")
            .join("2026")
            .join("03")
            .join("13");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            dir.path().join("session_index.jsonl"),
            concat!(
                "{\"id\":\"desktop-1\",\"thread_name\":\"Desktop\",\"updated_at\":\"2026-03-13T11:00:00Z\"}\n",
                "{\"id\":\"cli-1\",\"thread_name\":\"Cli\",\"updated_at\":\"2026-03-13T10:00:00Z\"}\n"
            ),
        )
        .unwrap();
        fs::write(
            sessions_dir.join("rollout-desktop.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-03-13T11:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"desktop-1\",\"timestamp\":\"2026-03-13T11:00:00Z\",\"cwd\":\"{}\",\"source\":\"vscode\"}}}}\n",
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();
        fs::write(
            sessions_dir.join("rollout-cli.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-03-13T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"cli-1\",\"timestamp\":\"2026-03-13T10:00:00Z\",\"cwd\":\"{}\",\"source\":\"exec\"}}}}\n",
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();

        let cli_only = list_environments_for_sources(dir.path(), 10, false, true, &[]).unwrap();
        assert_eq!(cli_only.len(), 1);
        assert_eq!(cli_only[0].latest_thread_id, Some("cli-1".to_string()));

        let desktop_only = list_environments_for_sources(dir.path(), 10, true, false, &[]).unwrap();
        assert_eq!(desktop_only.len(), 1);
        assert_eq!(
            desktop_only[0].latest_thread_id,
            Some("desktop-1".to_string())
        );
    }

    #[test]
    fn prefers_environment_name_from_workspace_config() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().join("workspace");
        let sessions_dir = dir.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(cwd.join(".codex").join("environments")).unwrap();
        fs::write(
            cwd.join(".codex")
                .join("environments")
                .join("environment.toml"),
            "version = 1\nname = \"Journal\"\n",
        )
        .unwrap();
        fs::write(
            sessions_dir.join("rollout-1.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"env-1\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"vscode\"}}}}\n",
                cwd.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();

        let environments = list_environments_for_sources(dir.path(), 10, true, false, &[]).unwrap();
        assert_eq!(environments[0].name, "Journal");
    }

    #[test]
    fn includes_seed_workspaces_without_history() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(workspace.join(".codex").join("environments")).unwrap();
        fs::write(
            workspace
                .join(".codex")
                .join("environments")
                .join("environment.toml"),
            "version = 1\nname = \"Seeded\"\n",
        )
        .unwrap();

        let environments =
            list_environments_for_sources(dir.path(), 10, false, false, &[workspace.clone()])
                .unwrap();

        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].cwd, normalize_path(workspace));
        assert_eq!(environments[0].name, "Seeded");
        assert_eq!(environments[0].latest_thread_id, None);
        assert_eq!(environments[0].updated_at, SEEDED_ENV_UPDATED_AT);
    }

    #[test]
    fn keeps_seed_workspaces_visible_when_limit_is_small() {
        let dir = tempdir().unwrap();
        let history_workspace = workspace_path(dir.path(), "history");
        let seed_workspace = dir.path().join("seed");
        let sessions_dir = dir.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(&history_workspace).unwrap();
        fs::create_dir_all(seed_workspace.join(".codex").join("environments")).unwrap();
        fs::write(
            sessions_dir.join("rollout-1.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"env-1\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"exec\"}}}}\n",
                escaped_json_path(&history_workspace)
            ),
        )
        .unwrap();

        let environments =
            list_environments_for_sources(dir.path(), 1, false, true, &[seed_workspace.clone()])
                .unwrap();

        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].cwd, normalize_path(seed_workspace));
        assert_eq!(environments[0].latest_thread_id, None);
    }

    #[test]
    fn uses_stable_selector_for_seeded_and_historical_environments() {
        let cwd = PathBuf::from("/tmp/workspace");
        let historical = CodexEnvironmentSummary {
            cwd: cwd.clone(),
            name: "History".to_string(),
            latest_thread_id: Some("thread-1".to_string()),
            updated_at: "2026-03-13T09:00:00Z".to_string(),
        };
        let seeded = CodexEnvironmentSummary {
            cwd,
            name: "Seeded".to_string(),
            latest_thread_id: None,
            updated_at: SEEDED_ENV_UPDATED_AT.to_string(),
        };

        assert_eq!(
            environment_selector_key(&historical),
            environment_selector_key(&seeded)
        );
    }

    #[test]
    fn uses_first_user_prompt_as_thread_title_when_index_name_is_missing() {
        let dir = tempdir().unwrap();
        let cwd = workspace_path(dir.path(), "telecodex");
        let sessions_dir = dir.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            sessions_dir.join("rollout-1.jsonl"),
            format!(
                concat!(
                    "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"preview-1\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"exec\"}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:01Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"Do not start a new session\\n\\nFollow these instructions for this turn:\\nignore me\"}}}}\n"
                ),
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();

        let threads = list_threads_for_cwd(dir.path(), &cwd, 10).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].title, "Do not start a new session");
    }

    #[test]
    fn collapses_git_worktrees_into_one_environment() {
        let dir = tempdir().unwrap();
        let main_repo = dir.path().join("repos").join("robin2");
        let worktree = dir
            .path()
            .join("codex")
            .join("worktrees")
            .join("48ad")
            .join("robin2");
        let git_worktree_dir = main_repo.join(".git").join("worktrees").join("robin22");
        fs::create_dir_all(&git_worktree_dir).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", git_worktree_dir.display()),
        )
        .unwrap();
        fs::write(git_worktree_dir.join("commondir"), "../..\n").unwrap();
        fs::write(
            git_worktree_dir.join("gitdir"),
            worktree.join(".git").display().to_string(),
        )
        .unwrap();

        assert_eq!(
            environment_identity_for_cwd(&main_repo),
            normalize_path(main_repo.clone())
        );
        assert_eq!(
            environment_identity_for_cwd(&worktree),
            normalize_path(main_repo)
        );
    }

    #[test]
    fn finds_thread_by_prefix() {
        let dir = tempdir().unwrap();
        let cwd = workspace_path(dir.path(), "telecodex");
        let sessions_dir = dir.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            sessions_dir.join("rollout-1.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"019ce672-9445-7612-bc5e-c8243a0d1915\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"exec\"}}}}\n",
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();

        let thread = find_thread_by_prefix(dir.path(), &cwd, "019ce672")
            .unwrap()
            .expect("thread");
        assert_eq!(thread.id, "019ce672-9445-7612-bc5e-c8243a0d1915");
    }

    #[test]
    fn finds_thread_by_short_display_selector() {
        let dir = tempdir().unwrap();
        let cwd = workspace_path(dir.path(), "codebot");
        let sessions_dir = dir.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            sessions_dir.join("rollout-1.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"019cd417-2272-7abc-8def-0abc01b61077\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"exec\"}}}}\n",
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();

        let thread = find_thread_by_prefix(dir.path(), &cwd, "019cd417…01b61077")
            .unwrap()
            .expect("thread");
        assert_eq!(thread.id, "019cd417-2272-7abc-8def-0abc01b61077");
    }

    #[test]
    fn reads_recent_thread_history_from_event_messages() {
        let dir = tempdir().unwrap();
        let cwd = workspace_path(dir.path(), "telecodex");
        let sessions_dir = dir.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            sessions_dir.join("rollout-1.jsonl"),
            format!(
                concat!(
                    "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"019ce672-9445-7612-bc5e-c8243a0d1915\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"exec\"}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:01Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"weather\\n\\nFollow these instructions for this turn:\\nfoo\"}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:02Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"working on it\",\"phase\":\"commentary\"}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:03Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"done\",\"phase\":\"final_answer\"}}}}\n"
                ),
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();

        let history =
            read_thread_history(dir.path(), "019ce672-9445-7612-bc5e-c8243a0d1915", 10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].text, "weather");
        assert_eq!(history[1].role, "assistant");
        assert_eq!(history[1].text, "done");
    }

    #[test]
    fn reads_recent_thread_history_from_response_messages_and_skips_commentary() {
        let dir = tempdir().unwrap();
        let cwd = workspace_path(dir.path(), "desktop-history");
        let sessions_dir = dir.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            sessions_dir.join("rollout-1.jsonl"),
            format!(
                concat!(
                    "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"desktop-thread\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"vscode\"}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:01Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"ping\"}}]}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:02Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"phase\":\"commentary\",\"content\":[{{\"type\":\"output_text\",\"text\":\"working\"}}]}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:03Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"phase\":\"final_answer\",\"content\":[{{\"type\":\"summary_text\",\"text\":\"mini\"}},{{\"type\":\"output_text\",\"text\":\"pong\"}}]}}}}\n"
                ),
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();

        let history = read_thread_history(dir.path(), "desktop-thread", 10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].text, "ping");
        assert_eq!(history[1].role, "assistant");
        assert_eq!(history[1].text, "pong");
    }

    #[test]
    fn reads_legacy_response_message_history_without_phase() {
        let dir = tempdir().unwrap();
        let cwd = workspace_path(dir.path(), "legacy-response-history");
        let sessions_dir = dir.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            sessions_dir.join("rollout-1.jsonl"),
            format!(
                concat!(
                    "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"legacy-response-thread\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"vscode\"}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:01Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"reasoning\",\"summary\":[{{\"type\":\"summary_text\",\"text\":\"thinking\"}}],\"content\":null}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:02Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"question\"}}]}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:03Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"answer\"}}]}}}}\n"
                ),
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();

        let history = read_thread_history(dir.path(), "legacy-response-thread", 10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].text, "question");
        assert_eq!(history[1].text, "answer");
    }

    #[test]
    fn lists_threads_from_archived_sessions() {
        let dir = tempdir().unwrap();
        let cwd = workspace_path(dir.path(), "archive-only");
        let archived_dir = archived_sessions_dir(dir.path());
        fs::create_dir_all(&archived_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            archived_dir.join("rollout-archive.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"archived-1\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"exec\"}}}}\n",
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();

        let threads = list_threads_for_cwd(dir.path(), &cwd, 10).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "archived-1");
    }

    #[test]
    fn prefers_active_session_copy_over_archived_duplicate() {
        let dir = tempdir().unwrap();
        let active_cwd = workspace_path(dir.path(), "active");
        let archived_cwd = workspace_path(dir.path(), "archived");
        let sessions_dir = dir.path().join("sessions");
        let archived_dir = archived_sessions_dir(dir.path());
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(&archived_dir).unwrap();
        fs::create_dir_all(&active_cwd).unwrap();
        fs::create_dir_all(&archived_cwd).unwrap();
        fs::write(
            archived_dir.join("rollout-archive.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-03-13T08:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"shared-thread\",\"timestamp\":\"2026-03-13T08:00:00Z\",\"cwd\":\"{}\",\"source\":\"vscode\"}}}}\n",
                escaped_json_path(&archived_cwd)
            ),
        )
        .unwrap();
        fs::write(
            sessions_dir.join("rollout-active.jsonl"),
            format!(
                "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"shared-thread\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"exec\"}}}}\n",
                escaped_json_path(&active_cwd)
            ),
        )
        .unwrap();

        let thread = find_thread_by_id(dir.path(), "shared-thread")
            .unwrap()
            .expect("thread");
        assert_eq!(thread.cwd, active_cwd);
        assert_eq!(thread.source, CodexHistorySource::Cli);
    }

    #[test]
    fn reads_history_from_archived_sessions() {
        let dir = tempdir().unwrap();
        let cwd = workspace_path(dir.path(), "archived-history");
        let archived_dir = archived_sessions_dir(dir.path());
        fs::create_dir_all(&archived_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            archived_dir.join("rollout-archive.jsonl"),
            format!(
                concat!(
                    "{{\"timestamp\":\"2026-03-13T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"archived-history\",\"timestamp\":\"2026-03-13T09:00:00Z\",\"cwd\":\"{}\",\"source\":\"exec\"}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:01Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"ping\"}}}}\n",
                    "{{\"timestamp\":\"2026-03-13T09:00:02Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"pong\",\"phase\":\"final_answer\"}}}}\n"
                ),
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();

        let history = read_thread_history(dir.path(), "archived-history", 10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].text, "ping");
        assert_eq!(history[1].text, "pong");
    }

    #[test]
    fn parses_legacy_rollout_with_environment_context() {
        let dir = tempdir().unwrap();
        let cwd = workspace_path(dir.path(), "legacy-home");
        let sessions_dir = dir.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            sessions_dir.join("rollout-legacy.jsonl"),
            format!(
                concat!(
                    "{{\"id\":\"5ae92be0-5ac6-44b4-aabc-cad1988c2087\",\"timestamp\":\"2025-08-22T23:09:32.674Z\",\"instructions\":null}}\n",
                    "{{\"record_type\":\"state\"}}\n",
                    "{{\"type\":\"message\",\"id\":null,\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"<environment_context>\\nCurrent working directory: {}\\nApproval policy: on-request\\nSandbox mode: workspace-write\\nNetwork access: restricted\\n</environment_context>\"}}]}}\n"
                ),
                escaped_json_path(&cwd)
            ),
        )
        .unwrap();

        let thread = find_thread_by_id(dir.path(), "5ae92be0-5ac6-44b4-aabc-cad1988c2087")
            .unwrap()
            .expect("legacy thread");
        assert_eq!(thread.cwd, cwd);
        assert_eq!(thread.updated_at, "2025-08-22T23:09:32.674Z");
        assert_eq!(thread.source, CodexHistorySource::Unknown);
    }
}
