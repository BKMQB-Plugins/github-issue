use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Default)]
struct Store(BTreeMap<String, (Vec<u8>, u64)>);

impl Store {
    fn seed(&mut self, alias: &str, value: &[u8], revision: u64) {
        self.0
            .insert(format!("repos/{alias}"), (value.to_vec(), revision));
    }
    fn run(&self, request: Value) -> Result<Response, Error> {
        invoke(
            EncodedValue {
                schema_version: "1.0".into(),
                content_type: "application/json".into(),
                data: serde_json::to_vec(&request).unwrap(),
            },
            |key| {
                Ok(self.0.get(key).map(|(value, revision)| StateValue {
                    value: value.clone(),
                    revision: *revision,
                }))
            },
            |prefix, limit| {
                assert_eq!(prefix, "repos/");
                assert_eq!(limit, 4096);
                Ok(self
                    .0
                    .iter()
                    .rev()
                    .take(limit as usize)
                    .map(|(key, (value, revision))| StateEntry {
                        key: key.clone(),
                        value: value.clone(),
                        revision: *revision,
                    })
                    .collect())
            },
        )
    }
    fn apply(&mut self, response: &Response) {
        for op in &response.state_ops {
            match op {
                StateOp::Put(put) => {
                    if let Some(revision) = put.expected_revision {
                        assert_eq!(self.0[&put.key].1, revision);
                    }
                    self.0.insert(put.key.clone(), (put.value.clone(), 99));
                }
                StateOp::Delete(delete) => {
                    assert_eq!(Some(self.0[&delete.key].1), delete.expected_revision);
                    self.0.remove(&delete.key);
                }
            }
        }
    }
}

fn request(operation: &str, id: Option<&str>, input: Value) -> Value {
    json!({"resource":"repositories", "operation":operation, "record_id":id, "input":input})
}
fn data(response: &Response) -> Value {
    serde_json::from_slice(&response.data.data).unwrap()
}
fn assert_error(result: Result<Response, Error>, code: ErrorCode) {
    assert_eq!(result.expect_err("expected error").code, code);
}

#[test]
fn crud_and_rename_reuse_chat_state_with_cas_including_revision_zero() {
    let mut store = Store::default();
    store.seed("CHAT", b"Owner/Existing.git", 0);
    let output = store
        .run(request(
            "update",
            Some("CHAT"),
            json!({"alias":"chat", "repository":"Owner/changed"}),
        ))
        .unwrap();
    let StateOp::Put(put) = &output.state_ops[0] else {
        panic!("put")
    };
    assert_eq!(put.expected_revision, Some(0));
    store.apply(&output);
    let output = store
        .run(request(
            "update",
            Some("CHAT"),
            json!({"alias":"renamed", "repository":"https://github.com/Owner/new.git/"}),
        ))
        .unwrap();
    assert_eq!(output.state_ops.len(), 2);
    store.apply(&output);
    assert!(!store.0.contains_key("repos/CHAT"));
    assert_eq!(store.0["repos/RENAMED"].0, b"Owner/new");
    let output = store
        .run(request(
            "create",
            None,
            json!({"alias":" fresh ", "repository":"owner/repo"}),
        ))
        .unwrap();
    store.apply(&output);
    let output = store
        .run(request("delete", Some("RENAMED"), Value::Null))
        .unwrap();
    assert_eq!(data(&output), json!({"records":[], "next_cursor":null}));
    store.apply(&output);
    assert_eq!(store.0.len(), 1);
    assert!(store.0.contains_key("repos/FRESH"));
}

#[test]
fn missing_and_colliding_records_are_not_overwritten() {
    let mut store = Store::default();
    store.seed("TAKEN", b"o/r", 7);
    store.seed("OLD", b"o/r", 8);
    for operation in ["create", "update"] {
        assert_error(
            store.run(request(
                operation,
                (operation == "update").then_some("OLD"),
                json!({"alias":"taken", "repository":"other/repo"}),
            )),
            ErrorCode::Conflict,
        );
    }
    for operation in ["update", "delete"] {
        assert_error(
            store.run(request(
                operation,
                Some("MISSING"),
                json!({"alias":"new", "repository":"o/r"}),
            )),
            ErrorCode::NotFound,
        );
    }
    assert_eq!(store.0.len(), 2);
}

#[test]
fn sorted_search_pagination_has_no_duplicates_or_omissions() {
    let mut store = Store::default();
    for i in 0..205 {
        store.seed(&format!("A{i:03}"), b"Owner/Repo", i);
    }
    let mut req = request("list", None, Value::Null);
    req["search"] = json!("oWnEr/rE");
    req["limit"] = json!(100);
    let mut ids = Vec::new();
    loop {
        let output = store.run(req.clone()).unwrap();
        assert!(output.state_ops.is_empty());
        let data = data(&output);
        ids.extend(
            data["records"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["id"].as_str().unwrap().to_owned()),
        );
        if data["next_cursor"].is_null() {
            break;
        }
        req["cursor"] = data["next_cursor"].clone();
    }
    assert_eq!(
        ids,
        (0..205).map(|i| format!("A{i:03}")).collect::<Vec<_>>()
    );
    req["cursor"] = Value::Null;
    req["search"] = json!("a204");
    assert_eq!(
        data(&store.run(req.clone()).unwrap())["records"][0]["id"],
        "A204"
    );
    req["cursor"] = json!("MISSING");
    assert_error(store.run(req), ErrorCode::InvalidArgument);
}

#[test]
fn invalid_stored_keys_and_values_fail_even_when_filtered_out() {
    for (alias, bytes) in [
        ("BAD", vec![255]),
        ("BAD", b"https://github.com/o/r".to_vec()),
        ("BAD", b"o/..".to_vec()),
        ("lower", b"o/r".to_vec()),
    ] {
        let mut store = Store::default();
        store.seed(alias, &bytes, 2);
        let mut req = request("list", None, Value::Null);
        req["search"] = json!("no-match");
        assert_error(store.run(req), ErrorCode::Internal);
        if alias == "BAD" {
            assert_error(
                store.run(request("delete", Some(alias), Value::Null)),
                ErrorCode::Internal,
            );
        }
    }
}

#[test]
fn bounded_scan_rejects_saturation_instead_of_returning_partial_data() {
    let mut store = Store::default();
    for i in 0..4095 {
        store.seed(&format!("A{i}"), b"o/r", 1);
    }
    assert!(store.run(request("list", None, Value::Null)).is_ok());
    store.seed("LAST", b"o/r", 1);
    assert_error(
        store.run(request("list", None, Value::Null)),
        ErrorCode::Internal,
    );
}

#[test]
fn validates_request_and_input_boundaries_before_queries() {
    let queries = RefCell::new(0);
    let mut invalid_requests = vec![
        json!({}),
        request(
            "create",
            None,
            json!({"alias":"bad alias", "repository":"o/r"}),
        ),
        request("create", None, json!({"alias":"a", "repository":"o/.."})),
        request(
            "create",
            None,
            json!({"alias":"a", "repository":"o/r", "extra":true}),
        ),
        request("update", Some("lower"), Value::Null),
    ];
    for (field, value) in [
        ("limit", json!(0)),
        ("limit", json!(101)),
        ("search", json!("x".repeat(257))),
        ("cursor", json!("x".repeat(513))),
        ("record_id", json!("x".repeat(257))),
    ] {
        let mut req = request("list", None, Value::Null);
        req[field] = value;
        invalid_requests.push(req);
    }
    for req in invalid_requests {
        let result = invoke(
            EncodedValue {
                schema_version: "1.0".into(),
                content_type: "application/json".into(),
                data: serde_json::to_vec(&req).unwrap(),
            },
            |_| {
                *queries.borrow_mut() += 1;
                Ok(None)
            },
            |_, _| {
                *queries.borrow_mut() += 1;
                Ok(vec![])
            },
        );
        assert_error(result, ErrorCode::InvalidArgument);
    }
    assert_eq!(*queries.borrow(), 0);
    let store = Store::default();
    assert_error(
        store.run(request("execute", None, Value::Null)),
        ErrorCode::Unsupported,
    );
    let mut req = request("list", None, Value::Null);
    req["resource"] = json!("unknown");
    assert_error(store.run(req), ErrorCode::Unsupported);
}
