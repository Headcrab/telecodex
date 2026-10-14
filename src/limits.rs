use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result};
use chrono::{Local, TimeZone};
use serde::Deserialize;

const PROGRESS_BAR_WIDTH: usize = 8;

#[derive(Debug, Clone, Deserialize)]
pub struct LimitsSnapshot {
    #[serde(alias = "limitId")]
    pub limit_id: Option<String>,
    #[serde(alias = "limitName")]
    pub limit_name: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub credits: Option<serde_json::Value>,
    #[serde(alias = "planType")]
    pub plan_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitWindow {
    #[serde(alias = "usedPercent")]
    pub used_percent: Option<f64>,
    #[serde(alias = "window_minutes", alias = "windowDurationMins")]
    pub window_minutes: Option<i64>,
    #[serde(alias = "resets_at", alias = "resetsAt")]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct EventEnvelope {
    timestamp: Option<String>,
    payload: Option<EventPayload>,
}

#[derive(Debug, Deserialize)]
struct EventPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    rate_limits: Option<LimitsSnapshot>,
}

pub fn default_codex_home() -> PathBuf {
    if let Some(codex_home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(codex_home);
    }

    #[cfg(windows)]
    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(user_profile).join(".codex");
    }

    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".codex");
    }

    PathBuf::from(".codex")
}

pub fn find_latest_limits_snapshot(codex_home: &Path) -> Result<Option<LimitsSnapshot>> {
    let mut candidates = Vec::new();
    collect_jsonl_files(&codex_home.join("sessions"), &mut candidates)?;
    collect_jsonl_files(&codex_home.join("archived_sessions"), &mut candidates)?;
    candidates.sort_by_key(|entry| std::cmp::Reverse(entry.1));

    let mut snapshots = Vec::new();
    for (path, _) in candidates.into_iter().take(200) {
        snapshots.extend(parse_snapshots_from_file(&path)?);
    }

    Ok(select_authoritative_snapshot(snapshots))
}

pub fn format_limits_summary(snapshot: &LimitsSnapshot) -> String {
    let mut parts = Vec::new();
    if let Some(primary) = &snapshot.primary {
        if let Some(summary) = format_window_inline("5h", primary) {
            parts.push(summary);
        }
    }
    if let Some(secondary) = &snapshot.secondary {
        if let Some(summary) = format_window_inline("7d", secondary) {
            parts.push(summary);
        }
    }
    if parts.is_empty() {
        let plan = snapshot.plan_type.as_deref().unwrap_or("unknown");
        let limit_name = snapshot
            .limit_name
            .as_deref()
            .or(snapshot.limit_id.as_deref())
            .unwrap_or("codex");
        let credits = snapshot
            .credits
            .as_ref()
            .map(|value| format!(", credits `{value}`"))
            .unwrap_or_default();
        format!("Plan `{plan}` / `{limit_name}`{credits}. No local limit windows in snapshot.")
    } else {
        parts.join("\n")
    }
}

pub fn format_limits_inline(snapshot: &LimitsSnapshot) -> Option<String> {
    snapshot
        .secondary
        .as_ref()
        .and_then(|window| format_window_inline("7d", window))
}

fn collect_jsonl_files(root: &Path, files: &mut Vec<(PathBuf, SystemTime)>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_jsonl_files(&path, files)?;
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            files.push((path, metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
        }
    }
    Ok(())
}

fn format_window_inline(label: &str, window: &RateLimitWindow) -> Option<String> {
    let used = window.used_percent?;
    let remaining = remaining_percent(used);
    let bar = format_progress_bar(remaining);
    let reset = format_reset(window.resets_at);
    let icon = match label {
        "5h" => "🕔",
        "7d" => "📅",
        _ => "•",
    };
    Some(format!(
        "{icon} {label:<2} {bar} {:>3.0}% · {reset}",
        remaining
    ))
}

fn format_reset(timestamp: Option<i64>) -> String {
    let Some(timestamp) = timestamp else {
        return "unknown".to_string();
    };
    Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|value| value.format("%d.%m %H:%M").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

fn remaining_percent(used_percent: f64) -> f64 {
    (100.0 - used_percent).clamp(0.0, 100.0)
}

fn format_progress_bar(remaining_percent: f64) -> String {
    let filled = ((remaining_percent / 100.0) * PROGRESS_BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(PROGRESS_BAR_WIDTH);
    let empty = PROGRESS_BAR_WIDTH.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

fn parse_snapshots_from_file(path: &Path) -> Result<Vec<(Option<String>, LimitsSnapshot)>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read limits candidate {}", path.display()))?;
    let mut snapshots = Vec::new();
    for line in raw.lines() {
        let Ok(envelope) = serde_json::from_str::<EventEnvelope>(line) else {
            continue;
        };
        let Some(payload) = envelope.payload else {
            continue;
        };
        if payload.kind.as_deref() != Some("token_count") {
            continue;
        }
        if let Some(snapshot) = payload.rate_limits {
            snapshots.push((envelope.timestamp, snapshot));
        }
    }
    Ok(snapshots)
}

fn select_authoritative_snapshot(
    mut snapshots: Vec<(Option<String>, LimitsSnapshot)>,
) -> Option<LimitsSnapshot> {
    snapshots.sort_by(|left, right| right.0.cmp(&left.0));
    let (_, newest_snapshot) = snapshots.first()?.clone();
    let primary_reset = newest_snapshot
        .primary
        .as_ref()
        .and_then(|window| window.resets_at);
    let secondary_reset = newest_snapshot
        .secondary
        .as_ref()
        .and_then(|window| window.resets_at);

    let same_window = snapshots
        .into_iter()
        .map(|(_, snapshot)| snapshot)
        .filter(|snapshot| {
            snapshot
                .primary
                .as_ref()
                .and_then(|window| window.resets_at)
                == primary_reset
                && snapshot
                    .secondary
                    .as_ref()
                    .and_then(|window| window.resets_at)
                    == secondary_reset
        })
        .collect::<Vec<_>>();

    Some(merge_snapshots(same_window, newest_snapshot))
}

fn merge_snapshots(snapshots: Vec<LimitsSnapshot>, fallback: LimitsSnapshot) -> LimitsSnapshot {
    if snapshots.is_empty() {
        return fallback;
    }

    let mut merged = fallback;
    merged.primary = merge_window(
        snapshots
            .iter()
            .filter_map(|snapshot| snapshot.primary.clone()),
        merged.primary.clone(),
    );
    merged.secondary = merge_window(
        snapshots
            .iter()
            .filter_map(|snapshot| snapshot.secondary.clone()),
        merged.secondary.clone(),
    );
    merged
}

fn merge_window(
    windows: impl Iterator<Item = RateLimitWindow>,
    fallback: Option<RateLimitWindow>,
) -> Option<RateLimitWindow> {
    let mut merged = fallback?;
    for window in windows {
        match (merged.used_percent, window.used_percent) {
            (Some(current), Some(next)) if next > current => merged.used_percent = Some(next),
            (None, Some(next)) => merged.used_percent = Some(next),
            _ => {}
        }
        if merged.window_minutes.is_none() {
            merged.window_minutes = window.window_minutes;
        }
        if merged.resets_at.is_none() {
            merged.resets_at = window.resets_at;
        }
    }
    Some(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_limits_inline() {
        let snapshot = LimitsSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: Some(12.0),
                window_minutes: Some(300),
                resets_at: Some(1_772_881_542),
            }),
            secondary: Some(RateLimitWindow {
                used_percent: Some(55.0),
                window_minutes: Some(10080),
                resets_at: Some(1_773_428_970),
            }),
            credits: None,
            plan_type: Some("plus".to_string()),
        };

        let formatted = format_limits_inline(&snapshot).unwrap();
        let lines = formatted.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("📅 7d ████░░░░  45%"));

        let summary = format_limits_summary(&snapshot);
        let lines = summary.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("🕔 5h ███████░  88%"));
        assert!(lines[1].starts_with("📅 7d ████░░░░  45%"));

        let mut missing_weekly = snapshot.clone();
        missing_weekly.secondary = None;
        assert!(format_limits_inline(&missing_weekly).is_none());

        let mut missing_weekly_usage = snapshot;
        missing_weekly_usage
            .secondary
            .as_mut()
            .unwrap()
            .used_percent = None;
        assert!(format_limits_inline(&missing_weekly_usage).is_none());
    }

    #[test]
    fn selects_highest_used_within_same_reset_window() {
        let low = LimitsSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: Some(38.0),
                window_minutes: Some(300),
                resets_at: Some(111),
            }),
            secondary: Some(RateLimitWindow {
                used_percent: Some(81.0),
                window_minutes: Some(10080),
                resets_at: Some(222),
            }),
            credits: None,
            plan_type: Some("team".to_string()),
        };
        let high = LimitsSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: Some(46.0),
                window_minutes: Some(300),
                resets_at: Some(111),
            }),
            secondary: Some(RateLimitWindow {
                used_percent: Some(84.0),
                window_minutes: Some(10080),
                resets_at: Some(222),
            }),
            credits: None,
            plan_type: Some("team".to_string()),
        };

        let selected = select_authoritative_snapshot(vec![
            (Some("2026-03-12T05:46:56Z".to_string()), low),
            (Some("2026-03-12T05:46:14Z".to_string()), high),
        ])
        .unwrap();

        assert_eq!(
            selected
                .primary
                .as_ref()
                .and_then(|window| window.used_percent),
            Some(46.0)
        );
        assert_eq!(
            selected
                .secondary
                .as_ref()
                .and_then(|window| window.used_percent),
            Some(84.0)
        );
        let formatted = format_limits_inline(&selected).unwrap();
        let lines = formatted.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("📅 7d █░░░░░░░  16%"));
    }

    #[test]
    fn parses_app_server_camel_case_windows() {
        let snapshot: LimitsSnapshot = serde_json::from_value(serde_json::json!({
            "limitId": "codex",
            "primary": {
                "usedPercent": 55,
                "windowDurationMins": 300,
                "resetsAt": 1773410067
            },
            "secondary": {
                "usedPercent": 26,
                "windowDurationMins": 10080,
                "resetsAt": 1773978343
            },
            "credits": {
                "hasCredits": false,
                "unlimited": false,
                "balance": null
            },
            "planType": "team"
        }))
        .expect("parse app-server rate limits");

        assert_eq!(
            snapshot
                .primary
                .as_ref()
                .and_then(|window| window.used_percent),
            Some(55.0)
        );
        assert_eq!(
            snapshot
                .secondary
                .as_ref()
                .and_then(|window| window.used_percent),
            Some(26.0)
        );
        let formatted = format_limits_summary(&snapshot);
        assert!(formatted.contains("🕔 5h"));
        assert!(formatted.contains("📅 7d"));
    }
}
