use super::*;
use std::thread;
use syzygy_runtime_contracts::model::{FunctionTool, ToolResult};
use tiny_http::{Header, Request, Response, Server};

fn provider(server: &Server) -> OpenRouter {
    let tools = [ToolNamespace {
        name: "workspace".into(),
        description: "Workspace".into(),
        tools: vec![Tool::Function(FunctionTool {
            name: "read".into(),
            description: "Read text".into(),
            parameters: json!({"type":"object"}),
            strict: false,
        })],
    }];
    let mut provider = OpenRouter::new(
        "fixture-key",
        "provider/model",
        &tools,
        "Use workspace tools",
        Some("medium"),
    )
    .unwrap();
    provider.endpoint = format!("http://{}/api/v1/chat/completions", server.server_addr());
    provider
}

fn request(server: &Server) -> (Request, Value) {
    let mut request = server
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .expect("mock request");
    assert_eq!(request.url(), "/api/v1/chat/completions");
    assert_eq!(
        request
            .headers()
            .iter()
            .find(|h| h.field.equiv("authorization"))
            .unwrap()
            .value
            .as_str(),
        "Bearer fixture-key"
    );
    let data = serde_json::from_reader(request.as_reader()).unwrap();
    (request, data)
}

fn send(request: Request, chunks: &[Value], done: bool) {
    let mut body = String::from(": heartbeat\n\n");
    for chunk in chunks {
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    if done {
        body.push_str("data: [DONE]\n\n");
    }
    request
        .respond(
            Response::from_string(body)
                .with_header(Header::from_bytes("Content-Type", "text/event-stream").unwrap()),
        )
        .unwrap();
}

#[test]
fn streaming_replays_namespaced_calls_results_and_reasoning_across_requests() {
    let server = Server::http("127.0.0.1:0").unwrap();
    let mut provider = provider(&server);
    let worker = thread::spawn(move || {
        let (first, body) = request(&server);
        assert_eq!(body["model"], "provider/model");
        assert_eq!(body["reasoning"]["effort"], "medium");
        assert_eq!(
            body["messages"][0],
            json!({"role":"system","content":"Use workspace tools"})
        );
        assert_eq!(
            body["messages"][1],
            json!({"role":"user","content":"Read a file"})
        );
        assert_eq!(body["tools"][0]["function"]["name"], "workspace__read");
        let block = json!({"type":"reasoning.encrypted","data":"opaque","format":"test"});
        send(
            first,
            &[
                json!({"id":"r1","choices":[{"index":0,"delta":{"reasoning_details":[block],"tool_calls":[{"index":0,"id":"c","type":"function","function":{"name":"workspace__","arguments":"{\"path\":"}}]}}]}),
                json!({"id":"r1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"read","arguments":"\"src/lib.rs\"}"}}]},"finish_reason":"tool_calls"}]}),
            ],
            true,
        );
        let (second, body) = request(&server);
        assert_eq!(body["messages"][2]["reasoning_details"], json!([block]));
        assert_eq!(
            body["messages"][2]["tool_calls"],
            json!([{"id":"c","type":"function","function":{"name":"workspace__read","arguments":"{\"path\":\"src/lib.rs\"}"}}])
        );
        assert_eq!(
            body["messages"][3],
            json!({"role":"tool","tool_call_id":"c","content":"contents"})
        );
        send(
            second,
            &[
                json!({"id":"r2","choices":[{"index":0,"delta":{"content":"Read "}}]}),
                json!({"id":"r2","choices":[{"index":0,"delta":{"content":"it."},"finish_reason":"stop"}],"usage":{"completion_tokens":2}}),
            ],
            true,
        );
    });
    let mut history = vec![ModelItem::User("Read a file".into())];
    let first = provider.respond(&history, &mut |_| {}).unwrap();
    assert_eq!(first.calls[0].namespace.as_deref(), Some("workspace"));
    assert_eq!(first.calls[0].name, "read");
    assert!(!first.replay.to_string().contains("fixture-key"));
    history.push(ModelItem::Assistant {
        reply: first,
        results: vec![ToolResult {
            call_id: "c".into(),
            output: ToolOutput::Text("contents".into()),
        }],
    });
    let mut text = String::new();
    let reply = provider
        .respond(&history, &mut |delta| text.push_str(&delta.text))
        .unwrap();
    assert_eq!(reply.text, "Read it.");
    assert_eq!(text, reply.text);
    assert_eq!(reply.replay["usage"]["completion_tokens"], 2);
    worker.join().unwrap();
    let other_key =
        OpenRouter::new("different-account-key", "provider/model", &[], "", None).unwrap();
    assert!(other_key.request(&history).is_err());
    let same_key = OpenRouter::new("fixture-key", "provider/model", &[], "", None).unwrap();
    assert!(same_key.request(&history).is_ok());
    let mut unbound = history.clone();
    if let ModelItem::Assistant { reply, .. } = &mut unbound[1] {
        reply.replay.as_object_mut().unwrap().remove("connection");
    }
    assert!(same_key.request(&unbound).is_err());
    provider.model = "different".into();
    assert!(provider.request(&history).is_err());
}

#[test]
fn interrupted_truncated_and_unknown_tool_completions_cannot_dispatch() {
    let server = Server::http("127.0.0.1:0").unwrap();
    let mut provider = provider(&server);
    let worker = thread::spawn(move || {
        for (name, reason, done) in [
            ("workspace__read", "tool_calls", false),
            ("workspace__read", "length", true),
            ("unsupported", "tool_calls", true),
        ] {
            let (request, _) = request(&server);
            send(
                request,
                &[
                    json!({"id":"r","choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"c","type":"function","function":{"name":name,"arguments":"{}"}
            }]},"finish_reason":reason}]}),
                ],
                done,
            );
        }
        let (request, _) = request(&server);
        send(
            request,
            &[
                json!({"id":"one","choices":[{"index":0,"delta":{"content":"partial"}}]}),
                json!({"id":"two","choices":[{"index":0,"delta":{"content":" other response"},"finish_reason":"stop"}]}),
            ],
            true,
        );
    });
    for _ in 0..4 {
        assert!(
            provider
                .respond(&[ModelItem::User("Hi".into())], &mut |_| {})
                .is_err()
        );
    }
    worker.join().unwrap();
}
