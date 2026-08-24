wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

use bkm::plugin::{queries, types};
use exports::bkm::plugin::{handler::Guest as HandlerGuest, lifecycle::Guest as LifecycleGuest};
use serde::Deserialize;
use serde_json::json;
use types::{
    ActionCompleted, ActionStatus, Command, CommandPayload, ConfigEntry, Disposition,
    EventEnvelope, EventPayload, HandlerOutput, HealthStatus, HttpHeader, HttpRequestCommand,
    InitContext, MessageReplyCommand, MessageTarget, MigrationOutput, PluginError, PluginErrorCode,
    PutState, StateEntry, StateOp,
};

const ALIAS_PREFIX: &str = "repos/";
const CREATE_ISSUE_COMMAND_ID: &str = "create-issue";

struct GithubIssuePlugin;

impl LifecycleGuest for GithubIssuePlugin {
    fn validate_config(config: Vec<ConfigEntry>) -> Result<(), PluginError> {
        let admins = config_value::<Vec<String>>(&config, "admins")?.unwrap_or_default();
        if admins.is_empty() || admins.iter().any(|id| id.trim().is_empty()) {
            return Err(invalid_config("`admins` must contain at least one user ID"));
        }
        let _ = config_value::<bool>(&config, "allow_all_submitters")?;
        Ok(())
    }

    fn init(_context: InitContext) -> Result<(), PluginError> {
        Ok(())
    }

    fn health() -> Result<HealthStatus, PluginError> {
        Ok(HealthStatus::Healthy)
    }

    fn migrate_state(
        _from_version: u32,
        _to_version: u32,
        _state: Vec<StateEntry>,
    ) -> Result<MigrationOutput, PluginError> {
        Ok(MigrationOutput {
            state_ops: Vec::new(),
        })
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }
}

impl HandlerGuest for GithubIssuePlugin {
    fn on_event(event: EventEnvelope) -> Result<HandlerOutput, PluginError> {
        match event.payload {
            EventPayload::MessageCreated(message) => {
                let text = message.text.trim();
                if command_name(text).is_some_and(|name| name.eq_ignore_ascii_case("whoami")) {
                    return Ok(handle_whoami(
                        &event.event_id,
                        &event.adapter_id,
                        &message.sender.id,
                        message.sender.display_name.as_deref(),
                        &message.target,
                    ));
                }
                if command_name(text).is_some_and(|name| name.eq_ignore_ascii_case("aliasRepo")) {
                    return handle_alias(&event.event_id, &message.sender.id, text);
                }
                if command_name(text).is_some_and(|name| name.eq_ignore_ascii_case("issue")) {
                    return handle_issue(
                        &event.event_id,
                        &event.adapter_id,
                        &message.sender.id,
                        message.sender.display_name.as_deref(),
                        &message.target,
                        &message.message_id,
                        text,
                    );
                }
                Ok(ignored())
            }
            EventPayload::ActionCompleted(completion) => {
                handle_action_completion(&event.event_id, completion)
            }
            _ => Ok(ignored()),
        }
    }
}

fn handle_whoami(
    event_id: &str,
    adapter_id: &str,
    sender_id: &str,
    display_name: Option<&str>,
    target: &MessageTarget,
) -> HandlerOutput {
    let (source_kind, source_id) = source_parts(target);
    let display_name = display_name
        .filter(|name| !name.trim().is_empty())
        .map_or_else(|| "（无昵称）".to_owned(), safe_metadata_value);
    reply(
        event_id,
        format!(
            "当前插件身份\n昵称：{display_name}\nSender ID：{sender_id}\n来源：{source_kind}\n来源 ID：{source_id}\nAdapter：{adapter_id}"
        ),
    )
}

fn handle_alias(event_id: &str, sender_id: &str, text: &str) -> Result<HandlerOutput, PluginError> {
    if !is_admin(sender_id)? {
        return Ok(reply(event_id, "只有插件管理员可以维护仓库别名。"));
    }
    let first_line = text.lines().next().unwrap_or(text);
    let mut parts = first_line.split_whitespace();
    let _command = parts.next();
    let Some(alias) = parts.next().and_then(normalize_alias) else {
        return Ok(reply(
            event_id,
            "用法：/aliasRepo <别名> https://github.com/<owner>/<repo>",
        ));
    };
    let Some(repository) = parts.next().and_then(parse_github_repository) else {
        return Ok(reply(
            event_id,
            "仓库地址必须是 https://github.com/<owner>/<repo>。",
        ));
    };
    if parts.next().is_some() {
        return Ok(reply(event_id, "仓库地址后不能包含其他参数。"));
    }
    let key = format!("{ALIAS_PREFIX}{alias}");
    let current = queries::state_get(&key).map_err(host_error)?;
    let expected_revision = current.map(|value| value.revision);
    Ok(HandlerOutput {
        disposition: Disposition::Continue,
        state_ops: vec![StateOp::Put(PutState {
            key,
            value: repository.as_bytes().to_vec(),
            expected_revision,
        })],
        commands: vec![reply_command(
            "alias-saved",
            event_id,
            format!("仓库别名 {alias} 已绑定到 https://github.com/{repository}"),
        )],
        diagnostics: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn handle_issue(
    event_id: &str,
    adapter_id: &str,
    sender_id: &str,
    display_name: Option<&str>,
    target: &MessageTarget,
    message_id: &str,
    text: &str,
) -> Result<HandlerOutput, PluginError> {
    let mut lines = text.lines();
    let first_line = lines.next().unwrap_or(text);
    let mut parts = first_line.split_whitespace();
    let _command = parts.next();
    let Some(alias) = parts.next().and_then(normalize_alias) else {
        return Ok(reply(
            event_id,
            "用法：/issue <仓库别名> <标题>，后续行作为正文。",
        ));
    };
    let title = parts.collect::<Vec<_>>().join(" ");
    if title.is_empty() || title.chars().count() > 256 {
        return Ok(reply(event_id, "Issue 标题不能为空且不能超过 256 个字符。"));
    }
    let user_body = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    let body = issue_body_with_source(
        &user_body,
        adapter_id,
        sender_id,
        display_name,
        target,
        message_id,
    );
    if body.len() > 60_000 {
        return Ok(reply(event_id, "Issue 正文不能超过 60000 字节。"));
    }
    if alias == "TEST" {
        return Ok(reply(
            event_id,
            issue_test_reply(&title, &user_body, sender_id, target),
        ));
    }
    if !can_submit(sender_id)? {
        return Ok(reply(event_id, "你没有通过此插件创建 Issue 的权限。"));
    }
    let key = format!("{ALIAS_PREFIX}{alias}");
    let Some(repository) = queries::state_get(&key).map_err(host_error)? else {
        return Ok(reply(
            event_id,
            format!("仓库别名 {alias} 不存在，请先使用 /aliasRepo 绑定。"),
        ));
    };
    let repository = String::from_utf8(repository.value)
        .map_err(|_| permanent("stored repository alias is not valid UTF-8"))?;
    Ok(HandlerOutput {
        disposition: Disposition::Continue,
        state_ops: Vec::new(),
        commands: vec![create_issue_command(event_id, &repository, &title, &body)?],
        diagnostics: Vec::new(),
    })
}

fn create_issue_command(
    event_id: &str,
    repository: &str,
    title: &str,
    body: &str,
) -> Result<Command, PluginError> {
    let request_body = serde_json::to_vec(&json!({
        "title": title,
        "body": body,
    }))
    .map_err(|error| permanent(&error.to_string()))?;
    Ok(Command {
        command_id: CREATE_ISSUE_COMMAND_ID.to_owned(),
        idempotency_key: Some(format!("{event_id}/github-issue")),
        deadline_ms: Some(15_000),
        payload: CommandPayload::HttpRequest(HttpRequestCommand {
            method: "POST".to_owned(),
            url: format!("https://api.github.com/repos/{repository}/issues"),
            headers: vec![
                HttpHeader {
                    name: "Accept".to_owned(),
                    value: "application/vnd.github+json".to_owned(),
                },
                HttpHeader {
                    name: "Content-Type".to_owned(),
                    value: "application/json".to_owned(),
                },
                HttpHeader {
                    name: "X-GitHub-Api-Version".to_owned(),
                    value: "2022-11-28".to_owned(),
                },
                HttpHeader {
                    name: "User-Agent".to_owned(),
                    value: "bkmqb-plugin-github-issue/0.2.5".to_owned(),
                },
            ],
            body: Some(request_body),
            timeout_ms: 15_000,
            max_response_bytes: 256 * 1024,
        }),
    })
}

fn handle_action_completion(
    event_id: &str,
    completion: ActionCompleted,
) -> Result<HandlerOutput, PluginError> {
    if completion.command_id != CREATE_ISSUE_COMMAND_ID || completion.kind != "http.request" {
        return Ok(ignored());
    }
    if completion.status != ActionStatus::Succeeded {
        let detail = completion
            .error_message
            .as_deref()
            .map(safe_excerpt)
            .unwrap_or_else(|| "请求未成功完成".to_owned());
        return Ok(reply(event_id, format!("GitHub Issue 创建失败：{detail}")));
    }
    let Some(result) = completion.result_data else {
        return Ok(reply(event_id, "GitHub Issue 创建失败：Host 未返回响应。"));
    };
    let response: HttpResponse = serde_json::from_slice(&result.data)
        .map_err(|error| permanent(&format!("invalid HTTP completion: {error}")))?;
    if response.status == 201 {
        let created: GithubIssueResponse = serde_json::from_slice(&response.body)
            .map_err(|error| permanent(&format!("invalid GitHub response: {error}")))?;
        return Ok(reply(
            event_id,
            format!(
                "✅ GitHub Issue 创建成功\n#{} {}\n{}",
                created.number, created.title, created.html_url
            ),
        ));
    }
    let message = serde_json::from_slice::<GithubErrorResponse>(&response.body)
        .ok()
        .map(|error| error.message)
        .unwrap_or_else(|| String::from_utf8_lossy(&response.body).into_owned());
    Ok(reply(
        event_id,
        format!(
            "❌ GitHub Issue 创建失败\nHTTP {}：{}",
            response.status,
            safe_excerpt(&message)
        ),
    ))
}

fn is_admin(sender_id: &str) -> Result<bool, PluginError> {
    Ok(config_query::<Vec<String>>("admins")?
        .unwrap_or_default()
        .iter()
        .any(|id| id == sender_id))
}

fn can_submit(sender_id: &str) -> Result<bool, PluginError> {
    Ok(config_query::<bool>("allow_all_submitters")?.unwrap_or(false) || is_admin(sender_id)?)
}

fn config_query<T: for<'de> Deserialize<'de>>(key: &str) -> Result<Option<T>, PluginError> {
    let value = queries::config_get(key).map_err(host_error)?;
    value
        .map(|value| {
            serde_json::from_slice(&value.data)
                .map_err(|error| permanent(&format!("invalid config `{key}`: {error}")))
        })
        .transpose()
}

fn config_value<T: for<'de> Deserialize<'de>>(
    config: &[ConfigEntry],
    key: &str,
) -> Result<Option<T>, PluginError> {
    config
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| {
            serde_json::from_slice(&entry.value.data)
                .map_err(|error| invalid_config(&format!("invalid `{key}`: {error}")))
        })
        .transpose()
}

fn command_name(text: &str) -> Option<&str> {
    text.lines()
        .next()?
        .split_whitespace()
        .next()
        .map(|command| command.trim_start_matches('/'))
}

fn issue_body_with_source(
    user_body: &str,
    adapter_id: &str,
    sender_id: &str,
    display_name: Option<&str>,
    target: &MessageTarget,
    message_id: &str,
) -> String {
    let (source_kind, source_id) = source_parts(target);
    let display_name = display_name
        .filter(|name| !name.trim().is_empty())
        .map_or_else(|| "unknown".to_owned(), safe_metadata_value);
    let metadata = format!(
        "---\nSubmitted via BroKnowMyQQBot\n\n- Sender: {display_name} (`{}`)\n- Source: {source_kind} (`{}`)\n- Adapter: `{}`\n- Message ID: `{}`",
        safe_metadata_value(sender_id),
        safe_metadata_value(source_id),
        safe_metadata_value(adapter_id),
        safe_metadata_value(message_id),
    );
    if user_body.is_empty() {
        metadata
    } else {
        format!("{user_body}\n\n{metadata}")
    }
}

fn issue_test_reply(title: &str, body: &str, sender_id: &str, target: &MessageTarget) -> String {
    let (_, source_id) = source_parts(target);
    format!(
        "✅ 测试通过\n标题：{}\n正文：\n{}\n发送者 ID：{}\n来源 ID：{}",
        title,
        body,
        safe_metadata_value(sender_id),
        safe_metadata_value(source_id),
    )
}

fn source_parts(target: &MessageTarget) -> (&'static str, &str) {
    match target {
        MessageTarget::Group(id) => ("Group", id),
        MessageTarget::Private(id) => ("Private", id),
        MessageTarget::Channel(id) => ("Channel", id),
        MessageTarget::GuildDirect(id) => ("GuildDirect", id),
    }
}

fn safe_metadata_value(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(character, '\r' | '\n'))
        .map(|character| if character == '`' { 'ˋ' } else { character })
        .take(256)
        .collect()
}

fn normalize_alias(alias: &str) -> Option<String> {
    let alias = alias.trim();
    if alias.is_empty()
        || alias.len() > 32
        || alias
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-'))
    {
        return None;
    }
    Some(alias.to_ascii_uppercase())
}

fn parse_github_repository(url: &str) -> Option<String> {
    let path = url
        .trim()
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")?;
    let mut segments = path.split('/');
    let owner = segments.next()?;
    let raw_repository = segments.next()?;
    if segments.next().is_some() {
        return None;
    }
    let repository = raw_repository
        .strip_suffix(".git")
        .unwrap_or(raw_repository);
    if !valid_repository_part(owner) || !valid_repository_part(repository) {
        return None;
    }
    Some(format!("{owner}/{repository}"))
}

fn valid_repository_part(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 100
        && part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn reply(event_id: &str, content: impl Into<String>) -> HandlerOutput {
    HandlerOutput {
        disposition: Disposition::Continue,
        state_ops: Vec::new(),
        commands: vec![reply_command("reply", event_id, content.into())],
        diagnostics: Vec::new(),
    }
}

fn reply_command(command_id: &str, event_id: &str, content: String) -> Command {
    Command {
        command_id: command_id.to_owned(),
        idempotency_key: Some(format!("{event_id}/{command_id}")),
        deadline_ms: Some(10_000),
        payload: CommandPayload::MessageReply(MessageReplyCommand { content }),
    }
}

fn ignored() -> HandlerOutput {
    HandlerOutput {
        disposition: Disposition::Ignore,
        state_ops: Vec::new(),
        commands: Vec::new(),
        diagnostics: Vec::new(),
    }
}

fn invalid_config(message: &str) -> PluginError {
    PluginError {
        code: PluginErrorCode::InvalidConfig,
        message: message.to_owned(),
        retryable: false,
    }
}

fn permanent(message: &str) -> PluginError {
    PluginError {
        code: PluginErrorCode::Permanent,
        message: message.to_owned(),
        retryable: false,
    }
}

fn host_error(error: types::HostError) -> PluginError {
    permanent(&format!("Host query failed: {}", error.message))
}

fn safe_excerpt(message: &str) -> String {
    message.chars().take(300).collect()
}

#[derive(Deserialize)]
struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

#[derive(Deserialize)]
struct GithubIssueResponse {
    html_url: String,
    number: u64,
    title: String,
}

#[derive(Deserialize)]
struct GithubErrorResponse {
    message: String,
}

#[cfg(test)]
mod tests {
    use super::{
        CommandPayload, MessageTarget, create_issue_command, issue_body_with_source,
        issue_test_reply, normalize_alias, parse_github_repository, source_parts,
    };

    #[test]
    fn normalizes_repository_aliases() {
        assert_eq!(normalize_alias("cdc").as_deref(), Some("CDC"));
        assert!(normalize_alias("bad alias").is_none());
    }

    #[test]
    fn identifies_guild_direct_sources() {
        assert_eq!(
            source_parts(&MessageTarget::GuildDirect("guild-id".to_owned())),
            ("GuildDirect", "guild-id")
        );
    }

    #[test]
    fn accepts_only_canonical_github_repository_urls() {
        assert_eq!(
            parse_github_repository("https://github.com/Jasons-impart/Create-Delight-Core")
                .as_deref(),
            Some("Jasons-impart/Create-Delight-Core")
        );
        assert_eq!(
            parse_github_repository("https://github.com/owner/repo.git/").as_deref(),
            Some("owner/repo")
        );
        assert!(parse_github_repository("http://github.com/owner/repo").is_none());
        assert!(parse_github_repository("https://github.com/owner/repo/issues").is_none());
    }

    #[test]
    fn creates_the_expected_github_api_request_without_an_authorization_header() {
        let issue_body = issue_body_with_source(
            "第一行\n第二行",
            "qq-official-main",
            "sender-open-id",
            Some("测试用户"),
            &MessageTarget::Group("group-open-id".to_owned()),
            "message-id",
        );
        let command = create_issue_command(
            "event-1",
            "Jasons-impart/Create-Delight-Core",
            "测试标题",
            &issue_body,
        )
        .unwrap();
        let CommandPayload::HttpRequest(request) = command.payload else {
            panic!("expected HTTP request");
        };
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.url,
            "https://api.github.com/repos/Jasons-impart/Create-Delight-Core/issues"
        );
        assert!(
            request
                .headers
                .iter()
                .all(|header| !header.name.eq_ignore_ascii_case("authorization"))
        );
        let body: serde_json::Value =
            serde_json::from_slice(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["title"], "测试标题");
        let body = body["body"].as_str().unwrap();
        assert!(body.starts_with("第一行\n第二行\n\n---"));
        assert!(body.contains("Sender: 测试用户 (`sender-open-id`)"));
        assert!(body.contains("Source: Group (`group-open-id`)"));
        assert!(body.contains("Adapter: `qq-official-main`"));
        assert!(body.contains("Message ID: `message-id`"));
    }

    #[test]
    fn test_alias_returns_a_clear_dry_run_reply() {
        let reply = issue_test_reply(
            "功能补全测试",
            "第一行\n第二行",
            "sender-id",
            &MessageTarget::Group("group-id".to_owned()),
        );
        assert_eq!(
            reply,
            "✅ 测试通过\n标题：功能补全测试\n正文：\n第一行\n第二行\n发送者 ID：sender-id\n来源 ID：group-id"
        );
    }
}

export!(GithubIssuePlugin);
