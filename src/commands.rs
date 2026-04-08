use anyhow::{Result, bail};

use crate::{config::SearchMode, models::ReviewRequest, telegram::BotCommand};

#[derive(Debug, Clone)]
pub struct CommandHelp {
    pub text: String,
    pub quick_commands: Vec<Vec<String>>,
}

#[derive(Debug, Clone)]
pub enum ParsedInput {
    Bridge(BridgeCommand),
    Forward(String),
}

#[derive(Debug, Clone)]
pub enum BridgeCommand {
    Login,
    Logout,
    New { title: Option<String> },
    Topic { title: Option<String> },
    Use { thread_id_prefix: String },
    Review(ReviewRequest),
    Cd { path: String },
    Pwd,
    Environments,
    Sessions,
    Status,
    Stop,
    Allow { user_id: i64 },
    Deny { user_id: i64 },
    Role { user_id: i64, role: String },
    Model { model: Option<String> },
    Think { level: Option<String> },
    Prompt { prompt: Option<String> },
    Approval { approval: String },
    Sandbox { sandbox: String },
    Search { mode: SearchMode },
    AddDir { path: String },
    Limits,
    Copy,
    Clear,
    RestartBot,
    Unsupported { command: String },
}

const FORWARDED_COMMANDS: &[&str] = &[
    "/help",
    "/doctor",
    "/prompts",
    "/memory",
    "/mentions",
    "/init",
    "/bug",
    "/config",
    "/compact",
    "/agents",
    "/diff",
];

const UNSUPPORTED_COMMANDS: &[&str] = &[
    "/theme",
    "/vim",
    "/statusline",
    "/browser",
    "/ide",
    "/notifications",
    "/terminal-setup",
];

pub fn parse_command(command: &str, args: &str, original_text: &str) -> Result<ParsedInput> {
    if FORWARDED_COMMANDS.contains(&command) {
        return Ok(ParsedInput::Forward(original_text.trim().to_string()));
    }
    if UNSUPPORTED_COMMANDS.contains(&command) {
        return Ok(ParsedInput::Bridge(BridgeCommand::Unsupported {
            command: command.to_string(),
        }));
    }

    let bridge = match command {
        "/login" => BridgeCommand::Login,
        "/logout" => BridgeCommand::Logout,
        "/new" => BridgeCommand::New {
            title: non_empty(args).map(ToOwned::to_owned),
        },
        "/topic" | "/new-topic" | "/new_topic" => BridgeCommand::Topic {
            title: non_empty(args).map(ToOwned::to_owned),
        },
        "/use" => BridgeCommand::Use {
            thread_id_prefix: required_arg(args, "/use <thread_id_prefix|latest>")?.to_string(),
        },
        "/review" => BridgeCommand::Review(parse_review_args(args)?),
        "/cd" => BridgeCommand::Cd {
            path: required_arg(args, "/cd <absolute_path>")?.to_string(),
        },
        "/pwd" => BridgeCommand::Pwd,
        "/environments" | "/envs" => BridgeCommand::Environments,
        "/sessions" => BridgeCommand::Sessions,
        "/status" => BridgeCommand::Status,
        "/stop" => BridgeCommand::Stop,
        "/allow" => BridgeCommand::Allow {
            user_id: parse_i64_arg(args, "/allow <tg_user_id>")?,
        },
        "/deny" => BridgeCommand::Deny {
            user_id: parse_i64_arg(args, "/deny <tg_user_id>")?,
        },
        "/role" => {
            let mut parts = args.split_whitespace();
            let user_id = parts
                .next()
                .ok_or_else(|| anyhow::anyhow!("/role <tg_user_id> <admin|user>"))?
                .parse::<i64>()?;
            let role = parts
                .next()
                .ok_or_else(|| anyhow::anyhow!("/role <tg_user_id> <admin|user>"))?
                .to_lowercase();
            BridgeCommand::Role { user_id, role }
        }
        "/model" => BridgeCommand::Model {
            model: non_empty(args).map(ToOwned::to_owned),
        },
        "/think" => BridgeCommand::Think {
            level: non_empty(args).map(ToOwned::to_owned),
        },
        "/prompt" => BridgeCommand::Prompt {
            prompt: non_empty(args).map(ToOwned::to_owned),
        },
        "/approval" => BridgeCommand::Approval {
            approval: required_arg(args, "/approval <never|on-request|untrusted>")?.to_string(),
        },
        "/sandbox" => BridgeCommand::Sandbox {
            sandbox: required_arg(
                args,
                "/sandbox <read-only|workspace-write|danger-full-access>",
            )?
            .to_string(),
        },
        "/search" => BridgeCommand::Search {
            mode: parse_search_mode(args)?,
        },
        "/add-dir" | "/add_dir" => BridgeCommand::AddDir {
            path: required_arg(args, "/add-dir <absolute_path>")?.to_string(),
        },
        "/limits" => BridgeCommand::Limits,
        "/copy" => BridgeCommand::Copy,
        "/clear" => BridgeCommand::Clear,
        "/restart_bot" => BridgeCommand::RestartBot,
        _ => return Ok(ParsedInput::Forward(original_text.trim().to_string())),
    };
    Ok(ParsedInput::Bridge(bridge))
}

pub fn command_help(command: &str, args: &str) -> Option<CommandHelp> {
    match command {
        "/approval" => Some(choice_help(
            "Approval policy",
            &[
                "/approval never",
                "/approval on-request",
                "/approval untrusted",
            ],
        )),
        "/sandbox" => Some(choice_help(
            "Sandbox mode",
            &[
                "/sandbox read-only",
                "/sandbox workspace-write",
                "/sandbox danger-full-access",
            ],
        )),
        "/search" => Some(choice_help(
            "Web search mode",
            &["/search on", "/search off", "/search cached"],
        )),
        "/think" => Some(choice_help(
            "Reasoning effort",
            &[
                "/think minimal",
                "/think low",
                "/think medium",
                "/think high",
                "/think default",
            ],
        )),
        "/role" => {
            let mut parts = args.split_whitespace();
            let first = parts.next();
            let second = parts.next();
            match (first, second) {
                (None, _) => Some(text_help(
                    "Usage: /role <tg_user_id> <admin|user>\n\nExamples:\n/role 123456789 admin\n/role 123456789 user",
                )),
                (Some(user_id), None) => Some(CommandHelp {
                    text: format!(
                        "Choose a role for `{user_id}`:\n/role {user_id} admin\n/role {user_id} user"
                    ),
                    quick_commands: vec![vec![
                        format!("/role {user_id} admin"),
                        format!("/role {user_id} user"),
                    ]],
                }),
                _ => None,
            }
        }
        "/allow" => Some(text_help(
            "Usage: /allow <tg_user_id>\n\nExample:\n/allow 123456789",
        )),
        "/deny" => Some(text_help(
            "Usage: /deny <tg_user_id>\n\nExample:\n/deny 123456789",
        )),
        "/cd" => Some(text_help(
            "Usage: /cd <absolute_path>\n\nExample:\n/cd /absolute/path/to/project",
        )),
        "/use" => Some(text_help(
            "Usage: /use <thread_id_prefix|latest>\n\nExamples:\n/use latest\n/use 019ce672",
        )),
        "/add-dir" | "/add_dir" => Some(text_help(
            "Usage: /add-dir <absolute_path>\n\nExample:\n/add-dir /absolute/path/to/workspace",
        )),
        "/review" => Some(text_help(
            "Usage: /review [--uncommitted] [--base <branch>] [--commit <sha>] [--title <title>] [prompt]\n\nExamples:\n/review\n/review --base main look for regressions\n/review --commit abc123 focus on bugs",
        )),
        _ => None,
    }
}

pub fn default_bot_commands() -> Vec<BotCommand> {
    vec![
        bot_command("help", "Show Codex help"),
        bot_command("status", "Show status for this session"),
        bot_command("login", "Log in to Codex with device code"),
        bot_command("logout", "Remove stored Codex credentials"),
        bot_command("new", "Start a fresh Codex session in this topic"),
        bot_command("topic", "Create a new Telegram topic from this session"),
        bot_command("cd", "Set session working directory"),
        bot_command("pwd", "Show current working directory"),
        bot_command("model", "Set or show current model"),
        bot_command("think", "Set or show reasoning effort"),
        bot_command("prompt", "Set or show session prompt"),
        bot_command("approval", "Set approval policy"),
        bot_command("sandbox", "Set sandbox mode"),
        bot_command("search", "Set web search mode"),
        bot_command("add_dir", "Add writable directory"),
        bot_command("limits", "Show latest Codex rate limits snapshot"),
        bot_command("review", "Run codex review"),
        bot_command("environments", "Show importable Codex environments"),
        bot_command("sessions", "List sessions in this chat"),
        bot_command("copy", "Resend the last assistant reply"),
        bot_command("clear", "Start a fresh Codex session on the next turn"),
        bot_command("stop", "Stop the active turn"),
        bot_command("restart_bot", "Admin: restart the bot process"),
        bot_command("allow", "Admin: allow a Telegram user"),
        bot_command("deny", "Admin: deny a Telegram user"),
        bot_command("role", "Admin: set user role"),
    ]
}

fn parse_review_args(args: &str) -> Result<ReviewRequest> {
    let mut base = None;
    let mut commit = None;
    let mut title = None;
    let mut uncommitted = false;
    let mut prompt_tokens = Vec::new();
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut idx = 0usize;
    while idx < tokens.len() {
        match tokens[idx] {
            "--base" => {
                idx += 1;
                base = Some(required_token(&tokens, idx, "--base <branch>")?.to_string());
            }
            "--commit" => {
                idx += 1;
                commit = Some(required_token(&tokens, idx, "--commit <sha>")?.to_string());
            }
            "--title" => {
                idx += 1;
                title = Some(required_token(&tokens, idx, "--title <title>")?.to_string());
            }
            "--uncommitted" => {
                uncommitted = true;
            }
            token => prompt_tokens.push(token.to_string()),
        }
        idx += 1;
    }

    if base.is_none() && commit.is_none() && !uncommitted {
        uncommitted = true;
    }

    Ok(ReviewRequest {
        base,
        commit,
        uncommitted,
        title,
        prompt: non_empty(&prompt_tokens.join(" ")).map(ToOwned::to_owned),
    })
}

fn parse_search_mode(value: &str) -> Result<SearchMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "live" => Ok(SearchMode::Live),
        "cached" => Ok(SearchMode::Cached),
        "off" | "disabled" => Ok(SearchMode::Disabled),
        _ => bail!("/search <on|off|cached>"),
    }
}

fn parse_i64_arg(value: &str, usage: &str) -> Result<i64> {
    required_arg(value, usage)?
        .parse::<i64>()
        .map_err(Into::into)
}

fn required_arg<'a>(value: &'a str, usage: &str) -> Result<&'a str> {
    non_empty(value).ok_or_else(|| anyhow::anyhow!("{}", usage))
}

fn required_token<'a>(tokens: &'a [&str], idx: usize, usage: &str) -> Result<&'a str> {
    tokens
        .get(idx)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("{}", usage))
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn bot_command(command: &str, description: &str) -> BotCommand {
    BotCommand {
        command: command.to_string(),
        description: description.to_string(),
    }
}

fn text_help(text: &str) -> CommandHelp {
    CommandHelp {
        text: text.to_string(),
        quick_commands: Vec::new(),
    }
}

fn choice_help(title: &str, commands: &[&str]) -> CommandHelp {
    let quick_commands = commands
        .chunks(2)
        .map(|chunk| chunk.iter().map(|value| (*value).to_string()).collect())
        .collect();
    CommandHelp {
        text: format!("{title}:\n{}", commands.join("\n")),
        quick_commands,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_role_command() {
        let parsed = parse_command("/role", "42 admin", "/role 42 admin").unwrap();
        match parsed {
            ParsedInput::Bridge(BridgeCommand::Role { user_id, role }) => {
                assert_eq!(user_id, 42);
                assert_eq!(role, "admin");
            }
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn defaults_review_to_uncommitted() {
        let review = parse_review_args("look for bugs").unwrap();
        assert!(review.uncommitted);
        assert_eq!(review.prompt.as_deref(), Some("look for bugs"));
    }

    #[test]
    fn parses_think_and_prompt_commands() {
        let think = parse_command("/think", "high", "/think high").unwrap();
        match think {
            ParsedInput::Bridge(BridgeCommand::Think { level }) => {
                assert_eq!(level.as_deref(), Some("high"));
            }
            _ => panic!("unexpected think variant"),
        }

        let prompt = parse_command("/prompt", "be concise", "/prompt be concise").unwrap();
        match prompt {
            ParsedInput::Bridge(BridgeCommand::Prompt { prompt }) => {
                assert_eq!(prompt.as_deref(), Some("be concise"));
            }
            _ => panic!("unexpected prompt variant"),
        }
    }

    #[test]
    fn parses_restart_bot_command() {
        let parsed = parse_command("/restart_bot", "", "/restart_bot").unwrap();
        match parsed {
            ParsedInput::Bridge(BridgeCommand::RestartBot) => {}
            _ => panic!("unexpected restart variant"),
        }
    }

    #[test]
    fn parses_login_and_logout_commands() {
        let login = parse_command("/login", "", "/login").unwrap();
        match login {
            ParsedInput::Bridge(BridgeCommand::Login) => {}
            _ => panic!("unexpected login variant"),
        }

        let logout = parse_command("/logout", "", "/logout").unwrap();
        match logout {
            ParsedInput::Bridge(BridgeCommand::Logout) => {}
            _ => panic!("unexpected logout variant"),
        }
    }

    #[test]
    fn provides_choice_help_for_fixed_value_commands() {
        let help = command_help("/sandbox", "").unwrap();
        assert!(help.text.contains("/sandbox read-only"));
        assert!(help.text.contains("/sandbox workspace-write"));
        assert!(help.text.contains("/sandbox danger-full-access"));
        assert_eq!(help.quick_commands.len(), 2);
    }

    #[test]
    fn provides_role_help_when_role_is_missing() {
        let help = command_help("/role", "123456789").unwrap();
        assert!(help.text.contains("/role 123456789 admin"));
        assert!(help.text.contains("/role 123456789 user"));
        assert_eq!(help.quick_commands.len(), 1);
    }

    #[test]
    fn provides_review_help() {
        let help = command_help("/review", "").unwrap();
        assert!(help.text.contains("Usage: /review"));
        assert!(help.text.contains("/review --base main"));
        assert!(help.text.contains("/review --commit abc123"));
    }

    #[test]
    fn default_bot_commands_are_parseable() {
        let cases = [
            ("/help", ParsedInputKind::Forward),
            ("/status", ParsedInputKind::Bridge),
            ("/login", ParsedInputKind::Bridge),
            ("/logout", ParsedInputKind::Bridge),
            ("/new test", ParsedInputKind::Bridge),
            ("/topic test", ParsedInputKind::Bridge),
            ("/cd /workspace/project", ParsedInputKind::Bridge),
            ("/pwd", ParsedInputKind::Bridge),
            ("/model gpt-5.4", ParsedInputKind::Bridge),
            ("/think high", ParsedInputKind::Bridge),
            ("/prompt be concise", ParsedInputKind::Bridge),
            ("/approval never", ParsedInputKind::Bridge),
            ("/sandbox workspace-write", ParsedInputKind::Bridge),
            ("/search on", ParsedInputKind::Bridge),
            ("/add_dir /workspace/shared", ParsedInputKind::Bridge),
            ("/limits", ParsedInputKind::Bridge),
            ("/review --uncommitted", ParsedInputKind::Bridge),
            ("/environments", ParsedInputKind::Bridge),
            ("/sessions", ParsedInputKind::Bridge),
            ("/copy", ParsedInputKind::Bridge),
            ("/clear", ParsedInputKind::Bridge),
            ("/stop", ParsedInputKind::Bridge),
            ("/restart_bot", ParsedInputKind::Bridge),
            ("/allow 123456789", ParsedInputKind::Bridge),
            ("/deny 123456789", ParsedInputKind::Bridge),
            ("/role 123456789 admin", ParsedInputKind::Bridge),
        ];

        let registered = default_bot_commands()
            .into_iter()
            .map(|command| command.command)
            .collect::<Vec<_>>();
        assert_eq!(registered.len(), cases.len());

        for (input, expected_kind) in cases {
            let (command, args) = input.split_once(' ').unwrap_or((input, ""));
            let parsed = parse_command(command, args, input).unwrap();
            match (parsed, expected_kind) {
                (ParsedInput::Forward(_), ParsedInputKind::Forward) => {}
                (ParsedInput::Bridge(_), ParsedInputKind::Bridge) => {}
                (other, expected) => {
                    panic!(
                        "unexpected parse result for {input}: got {other:?}, expected {expected:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn still_parses_hidden_use_command() {
        let parsed = parse_command("/use", "019ce672", "/use 019ce672").unwrap();
        match parsed {
            ParsedInput::Bridge(BridgeCommand::Use { thread_id_prefix }) => {
                assert_eq!(thread_id_prefix, "019ce672");
            }
            _ => panic!("unexpected use variant"),
        }
    }

    #[derive(Debug)]
    enum ParsedInputKind {
        Bridge,
        Forward,
    }
}
