use super::*;
use plugin_api::StateOp as HostStateOp;
use plugin_host::{CommitOptions, PluginStore, StoreError};

const INSTANCE: &str = "chat-regression-only";

fn chat_alias(store: &PluginStore, repository: &str) -> HandlerOutput {
    handle_alias_with_queries(
        "chat-event",
        &format!("new_alias https://github.com/{repository}"),
        "/",
        types::CallerAuthority {
            role: types::CallerRole::Owner,
            platform: "qq.official".into(),
            subject_hint: None,
        },
        |key| {
            Ok(store
                .state_get(INSTANCE, key)
                .unwrap()
                .map(|value| types::StateValue {
                    value: value.value,
                    revision: value.revision,
                }))
        },
    )
    .unwrap()
}

fn commit_chat(store: &PluginStore, output: &HandlerOutput) -> Result<(), StoreError> {
    let ops: Vec<_> = output
        .state_ops
        .iter()
        .map(|op| match op {
            StateOp::Put(put) => HostStateOp::Put {
                key: put.key.clone(),
                value: put.value.clone(),
                expected_revision: put.expected_revision,
            },
            StateOp::Delete(_) => panic!("alias chat must only put state"),
        })
        .collect();
    store.commit(
        INSTANCE,
        "chat-invocation",
        &ops,
        &[],
        CommitOptions::new(1024),
    )?;
    Ok(())
}

#[test]
fn missing_alias_is_created_and_existing_alias_updated_by_real_host() {
    let store = PluginStore::in_memory().unwrap();
    let created = chat_alias(&store, "owner/first");
    commit_chat(&store, &created).expect("chat must create a missing alias without CasConflict");
    let first = store
        .state_get(INSTANCE, "repos/NEW_ALIAS")
        .unwrap()
        .unwrap();
    assert_eq!(first.value, b"owner/first");
    let updated = chat_alias(&store, "owner/second");
    commit_chat(&store, &updated).expect("chat must update an existing alias");
    let second = store
        .state_get(INSTANCE, "repos/NEW_ALIAS")
        .unwrap()
        .unwrap();
    assert_eq!(second.value, b"owner/second");
    assert!(second.revision > first.revision);
    assert!(matches!(
        commit_chat(&store, &updated),
        Err(StoreError::CasConflict(_))
    ));
    assert_eq!(
        store
            .state_get(INSTANCE, "repos/NEW_ALIAS")
            .unwrap()
            .unwrap()
            .value,
        b"owner/second"
    );
}
