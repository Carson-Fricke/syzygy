use runtime::conversation::{
    Event, ModelReply, SqliteStore, Status, Store, Timeline, ToolCall, ToolInput,
};
use serde_json::json;
use std::{
    path::Path,
    process::{Command, Output},
};
use tempfile::tempdir;

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_syzygy"))
        .env("SYZYGY_HOME", home)
        .args(args)
        .output()
        .unwrap()
}

fn text(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn cli_can_inspect_and_reconcile_a_saved_turn_without_credentials() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    assert!(text(run(&home, &["list"])).is_empty());
    let db = home.join("chats.sqlite3");
    let mut timeline = Timeline::new(SqliteStore::open(&db).unwrap());
    timeline.bind("local",&json!({
        "provider":"chatgpt", "workspace":tmp.path(), "connection":"test-connection","model":"test-model"
    })).unwrap();
    let sequence = timeline
        .start("local", "turn", "Please read main.rs")
        .unwrap();
    drop(timeline);
    let mut store = SqliteStore::open(&db).unwrap();
    let reply = ModelReply {
        id: "reply".into(),
        text: "Reading".into(),
        replay: json!({"raw":"private-replay-only"}),
        calls: vec![ToolCall {
            call_id: "call".into(),
            name: "read".into(),
            namespace: Some("workspace".into()),
            input: ToolInput::Function {
                arguments: "{}".into(),
            },
        }],
    };
    let sequence = store
        .compare_and_append("local", sequence, "turn", &Event::Assistant { reply })
        .unwrap();
    store
        .compare_and_append(
            "local",
            sequence,
            "turn",
            &Event::ToolStarted {
                call_id: "call".into(),
            },
        )
        .unwrap();
    drop(store);

    let listed = text(run(&home, &["list"]));
    assert!(listed.contains("local") && listed.contains("test-model"));
    let displayed = text(run(&home, &["show", "local"]));
    assert!(displayed.contains("Please read main.rs") && displayed.contains("Started call"));
    assert!(!displayed.contains("private-replay-only"));
    let resumed = run(&home, &["resume", "local"]);
    assert!(!resumed.status.success());
    assert!(String::from_utf8_lossy(&resumed.stderr).contains("syzygy resolve"));
    text(run(
        &home,
        &["resolve", "local", "call", "--result", "verified receipt"],
    ));
    assert!(text(run(&home, &["show", "local"])).contains("verified receipt"));
    let check = Timeline::new(SqliteStore::open(db).unwrap());
    assert_eq!(check.status("local").unwrap(), Status::Runnable);
}
