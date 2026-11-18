use super::*;
use std::{fs, sync::Barrier};
use syzygy_adapter_sqlite::SqliteStore;
use syzygy_conversation::{RunOutcome, Timeline};
use tempfile::tempdir;

#[test]
fn saved_grant_rotates_once_and_resumed_sqlite_history_contains_only_replay_evidence() {
    let directory = tempdir().unwrap();
    let home = directory.path().join("profile");
    let account = AccountStore::at(&home).unwrap();
    let server = Server::http("127.0.0.1:0").unwrap();
    let client = local_client(&server);
    let fixture: Value = serde_json::from_str(include_str!("../codex_wire.json")).unwrap();
    let browser = Arc::new(Mutex::new(None));
    let state = browser.clone();
    let issuer = client.oauth.issuer.clone();
    let worker = thread::spawn(move || {
        grant(&server, &state, &issuer, "code", false);
        grant(&server, &state, &issuer, "refresh", false);
        for i in [0, 1] {
            let mut request = next(&server, "POST", "/v1/responses");
            expect_bearer(&mut request, "fixture-access-renewed");
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let json: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(json["input"], fixture["requests"][i]["input"]);
            reply_sse(request, fixture["streams"][i].as_array().unwrap());
        }
    });
    account
        .sign_in(&client, |url| {
            callback(url, &browser, false, CLIENT, 200);
            Ok(())
        })
        .unwrap();

    // Two independently opened host stores both observe the short-lived grant.
    // Only the first lock holder should call the token endpoint.
    let barrier = Barrier::new(2);
    let sessions = thread::scope(|scope| {
        let workers: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    let reopened = AccountStore::at(&home).unwrap();
                    barrier.wait();
                    reopened.session(&client).unwrap()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(sessions[0].replay_scope(), sessions[1].replay_scope());
    assert!(!sessions[0].needs_refresh());
    let grant_path = home.join("account.grant");
    #[cfg(windows)]
    assert!(!String::from_utf8_lossy(&fs::read(&grant_path).unwrap()).contains("fixture-access"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&grant_path).unwrap().permissions().mode() & 0o077,
            0
        );
    }

    let path = home.join("chats.sqlite3");
    let mut timeline = Timeline::new(SqliteStore::open(&path).unwrap());
    timeline
        .bind(
            "chat",
            &json!({"connection":sessions[0].replay_scope(),"model":"gpt-5.4"}),
        )
        .unwrap();
    timeline
        .start("chat", "turn", "Read the result for trial-7.")
        .unwrap();
    let options = RequestOptions::default();
    let mut provider = ResponsesProvider::new(&client, &sessions[0], "gpt-5.4", &options);
    let mut invoked = 0;
    let mut deltas = String::new();
    let outcome = timeline
        .run(
            "chat",
            &mut provider,
            &mut |call| {
                invoked += 1;
                assert_eq!(call.name, "read_trial");
                Ok(ToolOutput::Text(r#"{"passed":true}"#.into()))
            },
            &mut |delta| {
                if delta.channel == OutputChannel::Text {
                    deltas.push_str(&delta.text);
                }
            },
        )
        .unwrap();
    assert_eq!(outcome, RunOutcome::Complete);
    assert_eq!(invoked, 1);
    assert_eq!(deltas, "Trial seven passed.");
    worker.join().unwrap();
    drop(timeline);

    account.forget().unwrap();
    assert!(!grant_path.exists());
    assert!(AccountStore::at(&home).unwrap().session(&client).is_err());
    let reopened = Timeline::new(SqliteStore::open(&path).unwrap());
    let events = serde_json::to_string(&reopened.events("chat", 0, 100).unwrap()).unwrap();
    assert!(events.contains("Trial seven passed."));
    for credential in [
        "fixture-access",
        "fixture-refresh",
        "fixture@example.invalid",
        CALLBACK_CODE,
    ] {
        assert!(!events.contains(credential));
    }
}
