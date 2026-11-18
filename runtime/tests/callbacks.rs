use std::{
    env, fs,
    path::PathBuf,
    process::Command,
    time::{Duration, SystemTime},
};

use runtime::callbacks::{
    Admission, CallbackCommand, CallbackError, CallbackProtocolError, CallbackRequest,
    FileInvocationJournal, InvocationJournal, JournaledCallback, ProcessCallback, UnknownOutcome,
    WorkflowCallback,
};
use runtime::workflows::{PhaseExecution, execute_deterministic};
use serde_json::{Map, json};

struct Fixture(PathBuf);

#[test]
fn concurrent_claimants_see_one_complete_claim_and_never_repeat_a_legacy_unknown() {
    use std::sync::Barrier;
    let directory = Fixture(env::temp_dir().join(format!(
            "syzygy-claim-{}-{:?}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    fs::create_dir(&directory.0).unwrap();
    let journal = FileInvocationJournal::new(&directory.0);
    let request = CallbackRequest::new(
        "once",
        "test",
        None,
        Map::from_iter([("payload".into(), json!("x".repeat(65536)))]),
    )
    .unwrap();
    let start = Barrier::new(8);
    let winners = std::thread::scope(|threads| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                threads.spawn(|| {
                    start.wait();
                    match journal.admit(&request).unwrap() {
                        Admission::New => 1,
                        Admission::Pending => 0,
                        other => panic!("unexpected admission: {other:?}"),
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .sum::<usize>()
    });
    assert_eq!(winners, 1);
    let claim = directory
        .0
        .join(runtime::storage::document_identity("callback-", "legacy"))
        .with_extension("claim");
    fs::write(claim, b"{").unwrap();
    let legacy = CallbackRequest::new("legacy", "test", None, Map::new()).unwrap();
    assert!(matches!(
        journal.admit(&legacy).unwrap(),
        Admission::Pending
    ));
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn external_callbacks_join_durable_replies_and_retain_unknown_outcomes() {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        Fixture(env::temp_dir().join(format!("runtime-callback-{}-{nonce}", std::process::id())));
    fs::create_dir(&directory.0).unwrap();
    let executable = directory
        .0
        .join(format!("fixture{}", env::consts::EXE_SUFFIX));
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/callback.rs");
    let compiler = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    assert!(
        Command::new(compiler)
            .arg(fixture)
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap()
            .success()
    );

    let receipts = directory.0.join("receipts");
    let journal = directory.0.join("journal");
    let callback = |mode: &str, identity: &str, timeout| {
        let claim = journal
            .join(runtime::storage::document_identity("callback-", identity))
            .with_extension("claim");
        JournaledCallback::new(
            ProcessCallback::new(CallbackCommand::new(
                &executable,
                [
                    mode,
                    receipts.to_str().unwrap(),
                    identity,
                    claim.to_str().unwrap(),
                ],
                timeout,
            )),
            FileInvocationJournal::new(&journal),
        )
    };
    let request = |identity: &str, count: u8| {
        CallbackRequest::new(
            identity,
            "execute",
            Some("attempt-7".into()),
            Map::from_iter([("count".into(), json!(count))]),
        )
        .unwrap()
    };
    let deadline = Duration::from_secs(2);
    let accepted = request("accepted", 4);
    let result = execute_deterministic(
        || callback("success", "accepted", deadline).invoke(&accepted),
        || Ok(false),
        |output| {
            assert_eq!(output["request"]["phaseAttemptId"], "attempt-7");
            assert_eq!(output["request"]["payload"]["protocolVersion"], 2);
            assert_eq!(output["request"]["payload"]["count"], 4);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(result, PhaseExecution::Accepted);
    // Reopening the same journal returns the retained receipt without launching
    // the program or trusting a different payload under the same identity.
    assert!(
        callback("lost", "accepted", deadline)
            .invoke(&accepted)
            .is_ok()
    );
    assert!(matches!(
        callback("lost", "accepted", deadline).invoke(&request("accepted", 5)),
        Err(CallbackError::Unknown(UnknownOutcome::Protocol(
            CallbackProtocolError::InvocationConflict
        )))
    ));

    let lost = request("lost", 4);
    assert!(matches!(
        callback("lost", "lost", deadline).invoke(&lost),
        Err(CallbackError::Unknown(UnknownOutcome::Exit {
            code: Some(3)
        }))
    ));
    assert!(matches!(
        callback("success", "lost", deadline).invoke(&lost),
        Err(CallbackError::Unknown(UnknownOutcome::RetainedInvocation))
    ));
    let mismatch = request("mismatch", 4);
    assert!(matches!(
        callback("mismatch", "mismatch", deadline).invoke(&mismatch),
        Err(CallbackError::Unknown(UnknownOutcome::Protocol(
            CallbackProtocolError::MismatchedInvocation
        )))
    ));
    assert!(matches!(
        callback("success", "mismatch", deadline).invoke(&mismatch),
        Err(CallbackError::Unknown(UnknownOutcome::RetainedInvocation))
    ));
    assert_eq!(
        fs::read_to_string(&receipts)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["accepted", "lost", "mismatch"]
    );
    assert!(matches!(
        callback("timeout", "timeout", Duration::from_millis(200)).invoke(&request("timeout", 4)),
        Err(CallbackError::Unknown(UnknownOutcome::TimedOut))
    ));
}
