use crate::aliases::{
    ALIAS_PREFIX, normalize_alias, parse_github_repository, parse_repository_path,
};
use crate::exports::bkm::plugin::management::{Error, ErrorCode, Guest, Response};
use crate::types::{
    DeleteState, EncodedValue, HostError, PutState, StateEntry, StateOp, StateValue,
};
use crate::{GithubIssuePlugin, queries};
use serde::Deserialize;
use serde_json::{Value, json};

const MAX_SCAN: u32 = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    resource: String,
    operation: String,
    #[serde(default)]
    search: String,
    cursor: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
    record_id: Option<String>,
    input: Option<Value>,
}

fn default_limit() -> usize {
    20
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    alias: String,
    repository: String,
}

impl Guest for GithubIssuePlugin {
    fn invoke(request: EncodedValue) -> Result<Response, Error> {
        invoke(request, queries::state_get, queries::state_scan)
    }
}

// No lifecycle, authority impersonation or chat commands: Host authorizes and gates this call.
fn invoke(
    request: EncodedValue,
    mut get: impl FnMut(&str) -> Result<Option<StateValue>, HostError>,
    mut scan: impl FnMut(&str, u32) -> Result<Vec<StateEntry>, HostError>,
) -> Result<Response, Error> {
    if request.content_type != "application/json" || request.data.len() > 16 * 1024 {
        return Err(invalid("管理请求必须是大小不超过 16 KiB 的 JSON"));
    }
    let request: Request =
        serde_json::from_slice(&request.data).map_err(|_| invalid("管理请求格式不正确"))?;
    if request.resource != "repositories" {
        return Err(error(ErrorCode::Unsupported, "不支持此管理资源"));
    }
    if !(1..=100).contains(&request.limit)
        || request.search.len() > 256
        || request.cursor.as_ref().is_some_and(|s| s.len() > 512)
        || request.record_id.as_ref().is_some_and(|s| s.len() > 256)
    {
        return Err(invalid("分页、搜索或记录标识超出范围"));
    }
    if request.operation == "list" {
        return list(&request, scan(ALIAS_PREFIX, MAX_SCAN).map_err(query_error)?);
    }
    if !matches!(request.operation.as_str(), "create" | "update" | "delete") {
        return Err(error(ErrorCode::Unsupported, "不支持此管理操作"));
    }
    let mut state_ops = Vec::new();
    let old_alias = if request.operation == "create" {
        if request.record_id.is_some() {
            return Err(invalid("新建时不能指定旧记录"));
        }
        None
    } else {
        let alias = request
            .record_id
            .as_deref()
            .ok_or_else(|| invalid("缺少记录标识"))?;
        if normalize_alias(alias).as_deref() != Some(alias) {
            return Err(invalid("记录标识必须是大写别名"));
        }
        let key = format!("{ALIAS_PREFIX}{alias}");
        let current = get(&key)
            .map_err(query_error)?
            .ok_or_else(|| error(ErrorCode::NotFound, "仓库别名不存在"))?;
        stored_repository(&current.value)?;
        Some((alias, current.revision))
    };
    let data = if request.operation == "delete" {
        if request.input.is_some() {
            return Err(invalid("删除操作不能包含输入数据"));
        }
        let (alias, revision) = old_alias.expect("delete has an old record");
        state_ops.push(StateOp::Delete(DeleteState {
            key: format!("{ALIAS_PREFIX}{alias}"),
            expected_revision: Some(revision),
        }));
        json!({"records": [], "next_cursor": null})
    } else {
        let input: Input =
            serde_json::from_value(request.input.ok_or_else(|| invalid("缺少仓库别名数据"))?)
                .map_err(|_| invalid("必须提供别名和 GitHub 仓库，不能包含其他字段"))?;
        let alias = normalize_alias(&input.alias)
            .ok_or_else(|| invalid("别名须为 1 至 32 字节的字母、数字、下划线或短横线"))?;
        let repository = parse_github_repository(&input.repository)
            .or_else(|| parse_repository_path(input.repository.trim()))
            .ok_or_else(|| invalid("仓库须为 owner/repo 或 https://github.com/owner/repo"))?;
        let key = format!("{ALIAS_PREFIX}{alias}");
        let expected_revision = match old_alias {
            Some((old, revision)) if old == alias => Some(revision),
            _ => {
                // None means upsert, not insert-if-absent. Host's snapshot generation + gate
                // makes this missing check and the eventual transaction atomic against chat.
                if get(&key).map_err(query_error)?.is_some() {
                    return Err(error(ErrorCode::Conflict, "目标仓库别名已存在"));
                }
                None
            }
        };
        if let Some((old, revision)) = old_alias.filter(|(old, _)| *old != alias) {
            state_ops.push(StateOp::Delete(DeleteState {
                key: format!("{ALIAS_PREFIX}{old}"),
                expected_revision: Some(revision),
            }));
        }
        state_ops.push(StateOp::Put(PutState {
            key,
            value: repository.as_bytes().to_vec(),
            expected_revision,
        }));
        json!({"records": [{"id": alias, "values": {"alias": alias, "repository": repository}}], "next_cursor": null})
    };
    response(data, state_ops)
}

fn list(request: &Request, entries: Vec<StateEntry>) -> Result<Response, Error> {
    // state-scan has no continuation: reject a saturated scan instead of hiding later rows.
    if entries.len() >= MAX_SCAN as usize {
        return Err(error(
            ErrorCode::Internal,
            "仓库别名扫描达到 4096 条上限，无法保证完整列表",
        ));
    }
    let mut records = Vec::with_capacity(entries.len());
    let search = request.search.to_ascii_lowercase();
    for entry in entries {
        let alias = entry
            .key
            .strip_prefix(ALIAS_PREFIX)
            .filter(|alias| normalize_alias(alias).as_deref() == Some(*alias))
            .ok_or_else(|| error(ErrorCode::Internal, "存在无效的仓库别名状态键"))?;
        let repository = stored_repository(&entry.value)?;
        if alias.to_ascii_lowercase().contains(&search)
            || repository.to_ascii_lowercase().contains(&search)
        {
            records.push((alias.to_owned(), repository));
        }
    }
    records.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    if records.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(error(ErrorCode::Internal, "存在重复的仓库别名状态键"));
    }
    let start = match request.cursor.as_ref() {
        Some(cursor) => records
            .binary_search_by(|record| record.0.cmp(cursor))
            .map(|index| index + 1)
            .map_err(|_| invalid("分页游标无效，请重新读取列表"))?,
        None => 0,
    };
    let end = (start + request.limit).min(records.len());
    let cursor = (end < records.len()).then(|| records[end - 1].0.clone());
    let records: Vec<_> = records[start..end].iter().map(|(alias, repository)|
        json!({"id": alias, "values": {"alias": alias, "repository": repository}})).collect();
    response(
        json!({"records": records, "next_cursor": cursor}),
        Vec::new(),
    )
}

fn stored_repository(bytes: &[u8]) -> Result<String, Error> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(parse_repository_path)
        .ok_or_else(|| error(ErrorCode::Internal, "存在无效的仓库别名状态值"))
}

fn response(data: Value, state_ops: Vec<StateOp>) -> Result<Response, Error> {
    Ok(Response {
        data: EncodedValue {
            schema_version: "1.0".to_owned(),
            content_type: "application/json".to_owned(),
            data: serde_json::to_vec(&data)
                .map_err(|_| error(ErrorCode::Internal, "无法编码管理响应"))?,
        },
        state_ops,
    })
}

fn invalid(message: &str) -> Error {
    error(ErrorCode::InvalidArgument, message)
}
fn error(code: ErrorCode, message: &str) -> Error {
    Error {
        code,
        message: message.to_owned(),
    }
}
fn query_error(_: HostError) -> Error {
    error(ErrorCode::Internal, "无法读取仓库别名状态")
}

#[cfg(test)]
#[path = "management_tests.rs"]
mod tests;
