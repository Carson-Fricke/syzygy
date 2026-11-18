#![cfg(feature = "sqlite")]

use std::{
    io,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde_json::json;
use syzygy_conversation::{
    Error, Event, ModelItem, ModelReply, OutputDelta, Provider, Record, RunOutcome, SqliteStore,
    Status, Store, Timeline, ToolCall, ToolInput, ToolOutput,
};
use tempfile::tempdir;

struct Model<F>(F);

impl<F: FnMut(&[ModelItem]) -> ModelReply> Provider for Model<F> {
    type Error = io::Error;

    fn respond(
        &mut self,
        history: &[ModelItem],
        _: &mut dyn FnMut(OutputDelta),
    ) -> Result<ModelReply, Self::Error> {
        Ok(self.0(history))
    }
}

fn reply(id: &str, calls: &[&str]) -> ModelReply {
    ModelReply {
        id: id.to_owned(),
        text: format!("answer {id}"),
        calls: calls
            .iter()
            .map(|id| ToolCall {
                call_id: (*id).to_owned(),
                name: "read_file".into(),
                namespace: Some("workspace".into()),
                input: ToolInput::Function {
                    arguments: r#"{"path":"a.rs"}"#.into(),
                },
            })
            .collect(),
        replay: json!({"provider_evidence": id}),
    }
}

/// Simulate a backend which durably commits the item but loses its acknowledgement.
struct LoseAck {
    inner: SqliteStore,
    predicate: fn(&Event) -> bool,
}

impl Store for LoseAck {
    fn compare_and_append(
        &mut self,
        chat: &str,
        expected: u64,
        turn: &str,
        event: &Event,
    ) -> Result<u64, Error> {
        let sequence = self.inner.compare_and_append(chat, expected, turn, event)?;
        if (self.predicate)(event) {
            return Err(Error::Store(
                io::Error::other("acknowledgement lost").into(),
            ));
        }
        Ok(sequence)
    }

    fn read_after(&self, chat: &str, after: u64, limit: usize) -> Result<Vec<Record>, Error> {
        self.inner.read_after(chat, after, limit)
    }

    fn chats(&self) -> Result<Vec<String>, Error> {
        self.inner.chats()
    }
}

#[test]
fn lost_input_reply_joins_one_durable_runnable_record_after_reopen() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("chats.db");
    let mut first = Timeline::new(LoseAck {
        inner: SqliteStore::open(&path).unwrap(),
        predicate: |event| matches!(event, Event::User { .. }),
    });
    first.bind("chat", &json!({"workspace":"project"})).unwrap();
    assert!(matches!(
        first.start("chat", "turn-1", "hello"),
        Err(Error::Store(_))
    ));
    drop(first);
    let mut reopened = Timeline::new(SqliteStore::open(path).unwrap());
    assert_eq!(reopened.status("chat").unwrap(), Status::Runnable);
    assert_eq!(reopened.start("chat", "turn-1", "hello").unwrap(), 2);
    assert!(matches!(
        reopened.start("chat", "turn-1", "different"),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        reopened.bind("chat", &json!({"workspace":"different"})),
        Err(Error::Conflict)
    ));
    let page = reopened.events("chat", 1, 20).unwrap();
    assert_eq!(page.len(), 1);
    assert!(matches!(&page[0].event, Event::User { text } if text == "hello"));
}

#[test]
fn unknown_claim_stops_dispatch_until_explicit_reconciliation_and_survives_reopen() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("chats.db");
    let mut timeline = Timeline::new(LoseAck {
        inner: SqliteStore::open(&path).unwrap(),
        predicate: |event| matches!(event, Event::ToolStarted { .. }),
    });
    timeline.start("chat", "turn-1", "do two things").unwrap();
    let mut provider = Model(|_: &[ModelItem]| reply("r1", &["c1", "c2"]));
    assert!(matches!(
        timeline.run(
            "chat",
            &mut provider,
            &mut |_| panic!("claim acknowledgement was lost"),
            &mut |_| {}
        ),
        Err(Error::Store(_))
    ));
    drop(timeline);
    let mut reopened = Timeline::new(SqliteStore::open(&path).unwrap());
    assert_eq!(
        reopened.status("chat").unwrap(),
        Status::Uncertain {
            call_id: "c1".into()
        }
    );
    assert_eq!(
        reopened
            .run(
                "chat",
                &mut provider,
                &mut |_| panic!("cannot repeat tool"),
                &mut |_| {}
            )
            .unwrap(),
        RunOutcome::Uncertain {
            call_id: "c1".into()
        }
    );
    assert!(
        reopened
            .resolve("chat", "c2", ToolOutput::Text("wrong call".into()))
            .is_err()
    );
    let outcome = ToolOutput::Text("verified first result".into());
    reopened.resolve("chat", "c1", outcome.clone()).unwrap();
    reopened.resolve("chat", "c1", outcome.clone()).unwrap();
    assert!(matches!(
        reopened.resolve("chat", "c1", ToolOutput::Text("changed".into())),
        Err(Error::Conflict)
    ));
    let mut follow_up = Model(|history: &[ModelItem]| {
        let [
            ModelItem::User(_),
            ModelItem::Assistant {
                reply: saved,
                results,
            },
        ] = history
        else {
            panic!("unexpected history");
        };
        assert_eq!(saved.replay, json!({"provider_evidence":"r1"}));
        assert_eq!(results[0].call_id, "c1");
        assert_eq!(results[0].output, outcome);
        assert_eq!(results[1].call_id, "c2");
        reply("r2", &[])
    });
    let result = reopened
        .run(
            "chat",
            &mut follow_up,
            &mut |call| {
                assert_eq!(call.call_id, "c2");
                Ok(ToolOutput::Text("second result".into()))
            },
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(result, RunOutcome::Complete);
    drop(reopened);
    let mut final_open = Timeline::new(SqliteStore::open(path).unwrap());
    assert_eq!(final_open.status("chat").unwrap(), Status::Complete);
    assert_eq!(final_open.chats().unwrap(), vec!["chat"]);
    final_open.start("chat", "turn-2", "what next?").unwrap();
    let mut last = Model(|history: &[ModelItem]| {
        assert!(matches!(
            history,
            [
                ModelItem::User(_),
                ModelItem::Assistant { .. },
                ModelItem::Assistant { .. },
                ModelItem::User(_)
            ]
        ));
        reply("r3", &["new", "c1"])
    });
    assert!(matches!(
        final_open.run(
            "chat",
            &mut last,
            &mut |_| panic!("all calls must be validated first"),
            &mut |_| {}
        ),
        Err(Error::Invalid(_))
    ));
    assert_eq!(final_open.status("chat").unwrap(), Status::Runnable);
}

#[test]
fn concurrent_hosts_cannot_dispatch_from_the_same_snapshot_twice() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("chats.db");
    let mut timeline = Timeline::new(SqliteStore::open(&path).unwrap());
    timeline.start("chat", "turn", "please run").unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let effects = Arc::new(AtomicUsize::new(0));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let mut worker = Timeline::new(SqliteStore::open(&path).unwrap());
            let barrier = barrier.clone();
            let effects = effects.clone();
            std::thread::spawn(move || {
                let mut provider = Model(|history: &[ModelItem]| {
                    if matches!(history.last(), Some(ModelItem::User(_))) {
                        barrier.wait();
                        reply("r1", &["call"])
                    } else {
                        reply("r2", &[])
                    }
                });
                match worker.run(
                    "chat",
                    &mut provider,
                    &mut |_| {
                        effects.fetch_add(1, Ordering::SeqCst);
                        Ok(ToolOutput::Text("ok".into()))
                    },
                    &mut |_| {},
                ) {
                    Ok(RunOutcome::Complete) => true,
                    Err(Error::Conflict) => false,
                    other => panic!("unexpected worker result {other:?}"),
                }
            })
        })
        .collect();
    let successful = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .filter(|success| *success)
        .count();
    assert_eq!(successful, 1);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(timeline.status("chat").unwrap(), Status::Complete);
}

#[test]
fn process_exit_after_an_external_effect_preserves_the_claim() {
    const WORKER: &str = "SYZYGY_TEST_INTERRUPTED_TOOL_WORKSPACE";
    if let Some(directory) = std::env::var_os(WORKER) {
        let directory = std::path::PathBuf::from(directory);
        let mut worker = Timeline::new(SqliteStore::open(directory.join("chats.db")).unwrap());
        let mut provider = Model(|_: &[ModelItem]| reply("r1", &["write"]));
        worker
            .run(
                "chat",
                &mut provider,
                &mut |_| {
                    std::fs::write(directory.join("external-receipt"), "done").unwrap();
                    std::process::exit(47);
                },
                &mut |_| {},
            )
            .unwrap();
        panic!("fixture should exit while executing the tool");
    }
    let directory = tempdir().unwrap();
    let path = directory.path().join("chats.db");
    let mut timeline = Timeline::new(SqliteStore::open(&path).unwrap());
    timeline.start("chat", "turn", "write one receipt").unwrap();
    drop(timeline);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_exit_after_an_external_effect_preserves_the_claim",
        ])
        .env(WORKER, directory.path())
        .output()
        .unwrap();
    assert_eq!(
        status.status.code(),
        Some(47),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let mut resumed = Timeline::new(SqliteStore::open(path).unwrap());
    let mut provider = Model(|_: &[ModelItem]| reply("r2", &[]));
    assert_eq!(
        resumed
            .run(
                "chat",
                &mut provider,
                &mut |_| panic!("must not repeat"),
                &mut |_| {}
            )
            .unwrap(),
        RunOutcome::Uncertain {
            call_id: "write".into()
        }
    );
    let receipt = std::fs::read_to_string(directory.path().join("external-receipt")).unwrap();
    resumed
        .resolve("chat", "write", ToolOutput::Text(receipt))
        .unwrap();
    assert_eq!(
        resumed
            .run(
                "chat",
                &mut provider,
                &mut |_| panic!("already complete"),
                &mut |_| {}
            )
            .unwrap(),
        RunOutcome::Complete
    );
}
