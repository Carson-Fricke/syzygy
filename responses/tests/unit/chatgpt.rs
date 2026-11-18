//! Loopback tests of the public adapter and pinned App Server exchange.

#[path = "account_store.rs"]
mod account_store;

use std::{
    ops::ControlFlow,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use jsonwebtoken::{Algorithm, EncodingKey, Header as JwtHeader, encode};
use reqwest::blocking::get;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use syzygy_conversation::{ToolCall, ToolInput, ToolOutput, ToolResult};
use syzygy_runtime_contracts::OutputChannel;
use tiny_http::{Header, Request, Response, Server};

use super::*;

const CLIENT: &str = "oaiapp_syzygy_fixture";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const CALLBACK_CODE: &str = "fixture-authorization-code";

#[test]
fn signed_plan_session_matches_codex_http_tool_replay_and_streaming() {
    let fixture: Value = serde_json::from_str(include_str!("../codex_wire.json")).unwrap();
    let server = Server::http("127.0.0.1:0").unwrap();
    let client = local_client(&server);
    let browser = Arc::new(Mutex::new(None));
    let state = browser.clone();
    let expected = fixture.clone();
    let issuer = client.oauth.issuer.clone();
    let worker = thread::spawn(move || {
        grant(&server, &state, &issuer, "code", false);
        grant(&server, &state, &issuer, "refresh", false);

        let mut models = next(&server, "GET", "/v1/models");
        expect_bearer(&mut models, "fixture-access-renewed");
        reply_json(
            models,
            &json!({"models": [
                {"slug": "gpt-5.4", "display_name": "Fixture model", "visibility": "list"},
                {"slug": "hidden", "display_name": "Hidden", "visibility": "hide"}
            ]}),
        );

        for index in 0..2 {
            let mut request = next(&server, "POST", "/v1/responses");
            expect_bearer(&mut request, "fixture-access-renewed");
            assert_eq!(
                request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("accept"))
                    .unwrap()
                    .value
                    .as_str(),
                "text/event-stream"
            );
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&body).unwrap(),
                expected["requests"][index]
            );
            reply_sse(request, expected["streams"][index].as_array().unwrap());
        }
    });
    let host = HostId::default();
    let mut session = client
        .sign_in(&host, None, |url| {
            callback(url, &browser, true, CLIENT, 200);
            Ok(())
        })
        .unwrap();
    assert_eq!(session.email(), Some("fixture@example.invalid"));
    assert_eq!(session.connection_id(), CLIENT);
    assert!(session.needs_refresh());
    // Round-trip the credential record through the host's storage boundary.
    session = serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
    client.refresh(&mut session).unwrap();
    assert!(!session.needs_refresh());
    assert_eq!(client.models(&session).unwrap()[0].slug, "gpt-5.4");

    let mut history = vec![ModelItem::User("Read the result for trial-7.".into())];
    let options = RequestOptions {
        parallel_tool_calls: true,
        reasoning_effort: Some("medium".into()),
        reasoning_summary: Some("auto".into()),
        tools: vec![ToolNamespace {
            name: "research".into(),
            description: "Read retained research results".into(),
            tools: vec![Tool::Function(FunctionTool {
                name: "read_trial".into(),
                description: "Read a trial result".into(),
                strict: false,
                parameters: json!({
                    "type": "object",
                    "properties": {"trial": {"type": "string"}},
                    "required": ["trial"],
                    "additionalProperties": false
                }),
            })],
        }],
        ..RequestOptions::default()
    };
    let mut first_updates = Vec::new();
    let provider = ResponsesProvider::new(&client, &session, "gpt-5.4", &options);
    let initial = provider.prepare(&history).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(initial.body()).unwrap(),
        fixture["requests"][0]
    );
    let first = client
        .respond(&session, &initial, |update| {
            first_updates.push(update);
            ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(
        first.calls,
        vec![ToolCall {
            call_id: "call_fixture".into(),
            namespace: Some("research".into()),
            name: "read_trial".into(),
            input: ToolInput::Function {
                arguments: r#"{"trial":"trial-7"}"#.into()
            },
        }]
    );
    assert!(matches!(&first_updates[0], StreamEvent::Output(delta)
        if delta.channel == OutputChannel::Reasoning && delta.text == "Looking up the trial."));
    assert_eq!(
        first_updates
            .iter()
            .filter_map(|update| match update {
                StreamEvent::ToolInputDelta { item_id, delta } if item_id == "fc_fixture" =>
                    Some(delta.as_str()),
                _ => None,
            })
            .collect::<String>(),
        r#"{"trial":"trial-7"}"#
    );
    let first: ModelReply = serde_json::from_slice(&serde_json::to_vec(&first).unwrap()).unwrap();
    history.push(ModelItem::Assistant {
        reply: first,
        results: vec![ToolResult {
            call_id: "call_fixture".into(),
            output: ToolOutput::Text(r#"{"passed":true}"#.into()),
        }],
    });
    let mut text = Vec::new();
    let final_response = client
        .respond(&session, &provider.prepare(&history).unwrap(), |event| {
            if let StreamEvent::Output(delta) = event {
                assert_eq!(delta.channel, OutputChannel::Text);
                text.push(delta.text);
            }
            ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(json!(text), fixture["text_deltas"]);
    assert_eq!(final_response.text, "Trial seven passed.");
    assert_eq!(
        final_response.replay["usage"]["input_tokens_details"]["cached_tokens"],
        12
    );
    worker.join().unwrap();
}

#[test]
fn sign_in_rejects_wrong_nonce_and_missing_plan_grant() {
    for missing_scope in [false, true] {
        let server = Server::http("127.0.0.1:0").unwrap();
        let client = local_client(&server);
        let browser = Arc::new(Mutex::new(None));
        let state = browser.clone();
        let issuer = client.oauth.issuer.clone();
        let worker = thread::spawn(move || {
            grant(&server, &state, &issuer, "wrong_nonce", missing_scope);
        });
        let result = client.sign_in(&HostId::default(), None, |url| {
            callback(url, &browser, false, CLIENT, 400);
            Ok(())
        });
        assert!(matches!(result, Err(Error::SignIn(_))));
        worker.join().unwrap();
    }
}

#[test]
fn failed_or_interrupted_stream_never_returns_a_completion() {
    let session = fixture_session();
    let scope = session.replay_scope();
    let request = PreparedRequest::from_history(
        "gpt-5.4",
        None,
        &[ModelItem::User("Produce a result.".into())],
        &RequestOptions::default(),
    )
    .unwrap();
    let delta = json!({"type": "response.output_text.delta", "delta": "partial"});
    for failure in [
        json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded"}}}),
        json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}),
    ] {
        let bytes = sse(&[delta.clone(), failure]);
        let mut updates = 0;
        let result = crate::stream::decode(bytes.as_bytes(), &request, &scope, |_| {
            updates += 1;
            ControlFlow::Continue(())
        });
        assert!(matches!(result, Err(Error::Provider { .. })));
        assert_eq!(updates, 1);
    }
    let cutoff = sse(&[delta]);
    assert!(matches!(
        crate::stream::decode(cutoff.as_bytes(), &request, &scope, |_| {
            ControlFlow::Continue(())
        }),
        Err(Error::Interrupted)
    ));
    assert!(matches!(
        crate::stream::decode(cutoff.as_bytes(), &request, &scope, |_| ControlFlow::Break(
            ()
        )),
        Err(Error::Cancelled)
    ));
}

#[test]
fn parallel_function_and_custom_results_replay_in_provider_call_order() {
    let session = fixture_session();
    let scope = session.replay_scope();
    let mut history = vec![
        ModelItem::Developer("Use the available research tools.".into()),
        ModelItem::User("Look up and label the trial.".into()),
    ];
    let options = RequestOptions::default();
    let request = PreparedRequest::from_history("gpt-5.4", None, &history, &options).unwrap();
    let output = json!([
        {"type":"function_call","call_id":"p1","name":"lookup","namespace":"research","arguments":"{}"},
        {"type":"custom_tool_call","call_id":"p2","name":"label","namespace":"research","input":"flag trial 7"}
    ]);
    let events = sse(&[
        json!({"type":"response.output_item.done","output_index":0,"item":output[0]}),
        json!({"type":"response.output_item.done","output_index":1,"item":output[1]}),
        json!({"type":"response.completed","response":{"id":"r1"}}),
    ]);
    let completion = crate::stream::decode(events.as_bytes(), &request, &scope, |_| {
        ControlFlow::Continue(())
    });
    let completion = completion.unwrap();
    let mut results = vec![
        ToolResult {
            call_id: "p2".into(),
            output: ToolOutput::Text("labeled".into()),
        },
        ToolResult {
            call_id: "p1".into(),
            output: ToolOutput::Content(vec![json!({"type":"input_text","text":"found"})]),
        },
    ];
    history.push(ModelItem::Assistant {
        reply: completion.clone(),
        results: results.clone(),
    });
    assert!(PreparedRequest::from_history("gpt-5.4", None, &history, &options).is_err());
    results.reverse();
    history.pop();
    history.push(ModelItem::Assistant {
        reply: completion,
        results,
    });
    let next = PreparedRequest::from_history("gpt-5.4", None, &history, &options).unwrap();
    assert!(next.check_connection("another_account").is_err());
    assert!(PreparedRequest::from_history("another_model", None, &history, &options).is_err());
    let body: Value = serde_json::from_str(next.body()).unwrap();
    let input = body["input"].as_array().unwrap();
    assert_eq!(input[2..4], *output.as_array().unwrap());
    assert_eq!(input[4]["type"], "function_call_output");
    assert_eq!(input[4]["call_id"], "p1");
    assert_eq!(
        input[5],
        json!({"type":"custom_tool_call_output","call_id":"p2","output":"labeled"})
    );
    // A terminal provider event reusing a previous ID fails before callers receive a dispatchable reply.
    assert!(matches!(
        crate::stream::decode(events.as_bytes(), &next, &scope, |_| ControlFlow::Continue(
            ()
        )),
        Err(Error::Invalid(_))
    ));
}

fn local_client(server: &Server) -> ChatGpt {
    let mut client = ChatGpt::new().unwrap();
    let issuer = format!("http://{}", server.server_addr());
    client.api = format!("{issuer}/v1");
    client.oauth = auth::Endpoints {
        authorize: format!("{issuer}/authorize"),
        token: format!("{issuer}/oauth/token"),
        jwks: format!("{issuer}/jwks"),
        issuer,
    };
    client
}

fn browser_query(url: &Url) -> std::collections::HashMap<String, String> {
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "dynamic_agent_client");
    assert_eq!(query["agent_name_hint"], "Syzygy");
    assert_eq!(query["scope"], SCOPE);
    assert_eq!(query["resource"], API);
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(query["ext_agent_host_id"].starts_with("urn:uuid:"));
    assert!(query["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert!(query["redirect_uri"].ends_with("/auth/callback"));
    query
}

fn callback(
    url: &Url,
    state: &Mutex<Option<std::collections::HashMap<String, String>>>,
    send_bad_state: bool,
    client_id: &str,
    status: u16,
) {
    let query = browser_query(url);
    let mut redirect = Url::parse(&query["redirect_uri"]).unwrap();
    let state_value = query["state"].clone();
    *state.lock().unwrap() = Some(query);
    let client_id = client_id.to_owned();
    thread::spawn(move || {
        if send_bad_state {
            let mut invalid = redirect.clone();
            invalid.query_pairs_mut().append_pair("state", "invalid");
            assert_eq!(get(invalid).unwrap().status(), 400);
        }
        redirect
            .query_pairs_mut()
            .append_pair("state", &state_value)
            .append_pair("client_id", &client_id)
            .append_pair("code", CALLBACK_CODE);
        assert_eq!(get(redirect).unwrap().status(), status);
    });
}

fn grant(
    server: &Server,
    state: &Mutex<Option<std::collections::HashMap<String, String>>>,
    issuer: &str,
    mode: &str,
    missing_scope: bool,
) {
    let mut request = next(server, "POST", "/oauth/token");
    let mut raw = String::new();
    request.as_reader().read_to_string(&mut raw).unwrap();
    let mut url = Url::parse("http://127.0.0.1").unwrap();
    url.set_query(Some(&raw));
    let form: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(form["client_id"], CLIENT);
    assert_eq!(form["resource"], API);
    let state = state.lock().unwrap();
    let query = state.as_ref().unwrap();
    let key: Value = serde_json::from_str(include_str!("../oauth_test_key.json")).unwrap();
    let refresh = mode == "refresh";
    if refresh {
        assert_eq!(form["grant_type"], "refresh_token");
        assert_eq!(form["refresh_token"], "fixture-refresh");
        assert!(!form.contains_key("scope"));
    } else {
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], CALLBACK_CODE);
        assert_eq!(form["redirect_uri"], query["redirect_uri"]);
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
            query["code_challenge"]
        );
    }
    let mut header = JwtHeader::new(Algorithm::RS256);
    header.kid = Some("syzygy-test".into());
    let expiry = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 120;
    let nonce = if mode == "wrong_nonce" {
        "invalid"
    } else {
        &query["nonce"]
    };
    let claims = json!({"iss": issuer, "aud": CLIENT, "sub": "fixture-account",
        "email": "fixture@example.invalid", "exp": expiry, "nonce": nonce});
    let der = STANDARD
        .decode(key["pkcs1_base64"].as_str().unwrap())
        .unwrap();
    let id_token = encode(&header, &claims, &EncodingKey::from_rsa_der(&der)).unwrap();
    let scope = if missing_scope {
        "openid profile email offline_access"
    } else {
        SCOPE
    };
    reply_json(
        request,
        &json!({
            "token_type": "Bearer", "id_token": id_token, "scope": scope,
            "access_token": if refresh { "fixture-access-renewed" } else { "fixture-access" },
            "refresh_token": if refresh { "fixture-refresh-renewed" } else { "fixture-refresh" },
            "expires_in": if refresh { 3600 } else { 30 }
        }),
    );
    if !missing_scope {
        reply_json(next(server, "GET", "/jwks"), &json!({"keys": [key["jwk"]]}));
    }
}

fn fixture_session() -> Session {
    serde_json::from_value(json!({
        "host": "d42b9d37-1907-43ef-8f50-7cb67eb0d00a",
        "client_id": CLIENT, "subject": "sample", "email": null,
        "id_token": "", "access_token": "", "refresh_token": "",
        "scope": SCOPE, "expires_at": 0
    }))
    .unwrap()
}

fn next(server: &Server, method: &str, path: &str) -> Request {
    let request = server
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .expect("missing HTTP request");
    assert_eq!(request.method().as_str(), method);
    assert_eq!(request.url(), path);
    request
}

fn expect_bearer(request: &mut Request, expected: &str) {
    let header = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("authorization"))
        .unwrap();
    assert_eq!(header.value.as_str(), format!("Bearer {expected}"));
}

fn reply_json(request: Request, value: &Value) {
    request
        .respond(
            Response::from_string(value.to_string())
                .with_header(Header::from_bytes("Content-Type", "application/json").unwrap()),
        )
        .unwrap();
}

fn reply_sse(request: Request, events: &[Value]) {
    request
        .respond(
            Response::from_string(sse(events))
                .with_header(Header::from_bytes("Content-Type", "text/event-stream").unwrap()),
        )
        .unwrap();
}

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect()
}
