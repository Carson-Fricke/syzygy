//! Drive a captured Codex exchange through the SQLite turn coordinator and restart.

use super::*;
use serde_json::{Value, json};
use std::{fs, thread, time::Duration};
use syzygy_conversation::{RunOutcome, SqliteStore, Timeline, ToolOutput};
use tiny_http::{Header, Response, Server};
use uuid::Uuid;

#[test]
fn interrupted_tool_is_reconciled_before_codex_wire_replay() {
    let fixture: Value = serde_json::from_str(include_str!("../codex_wire.json")).unwrap();
    let server = Server::http("127.0.0.1:0").unwrap();
    let mut client = ChatGpt::new().unwrap();
    client.api = format!("http://{}/v1", server.server_addr());
    let expected = fixture.clone();
    let worker =
        thread::spawn(move || {
            for index in 0..2 {
                let mut request = server
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.url(), "/v1/responses");
                let body: Value = serde_json::from_reader(request.as_reader()).unwrap();
                assert_eq!(body, expected["requests"][index]);
                let sse: String = expected["streams"][index]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|event| format!("data: {event}\n\n"))
                    .collect();
                request
                    .respond(Response::from_string(sse).with_header(
                        Header::from_bytes("Content-Type", "text/event-stream").unwrap(),
                    ))
                    .unwrap();
            }
        });
    let session: Session = serde_json::from_value(json!({
        "host":"d42b9d37-1907-43ef-8f50-7cb67eb0d00a", "client_id":"sample", "subject":"sample",
        "email":null,"id_token":"","access_token":"loopback","refresh_token":"",
        "scope":"chatgpt.tokens.use.direct", "expires_at":u64::MAX
    }))
    .unwrap();
    let namespace = &fixture["requests"][0]["tools"][0];
    let function = &namespace["tools"][0];
    let options = RequestOptions {
        tools: vec![ToolNamespace {
            name: namespace["name"].as_str().unwrap().into(),
            description: namespace["description"].as_str().unwrap().into(),
            tools: vec![Tool::Function(FunctionTool {
                name: function["name"].as_str().unwrap().into(),
                description: function["description"].as_str().unwrap().into(),
                parameters: function["parameters"].clone(),
                strict: false,
            })],
        }],
        parallel_tool_calls: true,
        reasoning_effort: Some("medium".into()),
        reasoning_summary: Some("auto".into()),
        ..Default::default()
    };
    let mut provider = ResponsesProvider::new(&client, &session, "gpt-5.4", &options);
    let folder = std::env::temp_dir().join(format!("syzygy-responses-{}", Uuid::new_v4()));
    fs::create_dir(&folder).unwrap();
    let path = folder.join("conversation.sqlite");
    let mut timeline = Timeline::new(SqliteStore::open(&path).unwrap());
    timeline
        .start("chat", "turn", "Read the result for trial-7.")
        .unwrap();
    let first = timeline
        .run(
            "chat",
            &mut provider,
            &mut |call| {
                assert_eq!(call.call_id, "call_fixture");
                Err("executor connection lost after dispatch".into())
            },
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(
        first,
        RunOutcome::Uncertain {
            call_id: "call_fixture".into()
        }
    );
    drop(timeline);

    let mut restored = Timeline::new(SqliteStore::open(&path).unwrap());
    let mut never_dispatch = |_: &syzygy_conversation::ToolCall| -> Result<ToolOutput, String> {
        panic!("previously claimed tool must never be redispatched")
    };
    assert_eq!(
        restored
            .run("chat", &mut provider, &mut never_dispatch, &mut |_| {})
            .unwrap(),
        first
    );
    restored
        .resolve(
            "chat",
            "call_fixture",
            ToolOutput::Text(r#"{"passed":true}"#.into()),
        )
        .unwrap();
    let mut text = Vec::new();
    let last = restored
        .run("chat", &mut provider, &mut never_dispatch, &mut |delta| {
            text.push(delta.text)
        })
        .unwrap();
    assert_eq!(last, RunOutcome::Complete);
    assert_eq!(json!(text), fixture["text_deltas"]);
    worker.join().unwrap();
    drop(restored);
    fs::remove_dir_all(folder).unwrap();
}
