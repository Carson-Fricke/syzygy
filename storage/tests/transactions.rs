use syzygy_runtime_storage::{
    AppendFailure, EventConflict, EventIntent, TimelineAppend, append, append_transaction,
    commit_fenced, reconcile_event,
};

#[derive(Default)]
struct Catalog {
    sequence: u64,
    item: Option<(u64, &'static str)>,
    pending_work: usize,
    commits: usize,
}

struct Input<'a> {
    catalog: &'a mut Catalog,
    payload: &'static str,
    fail_before_write: bool,
}

fn intent(payload: &str) -> EventIntent<&str, &str> {
    EventIntent {
        workspace: "workspace",
        conversation: "chat",
        thread: None,
        execution: None,
        id: "input-1",
        kind: "input",
        payload,
    }
}

impl TimelineAppend for Input<'_> {
    type Attempt = u64;
    type Document = u64;
    type Error = &'static str;

    fn prepare(&mut self) -> Result<u64, Self::Error> {
        Ok(self.catalog.sequence + 1)
    }

    fn commit(&mut self, position: &u64) -> Result<(), AppendFailure<Self::Error>> {
        self.catalog.commits += 1;
        if self.fail_before_write {
            return Err(AppendFailure::Unknown("disconnected"));
        }
        if self.catalog.item.is_some() {
            return Err(AppendFailure::Conflict("already accepted"));
        }
        self.catalog.item = Some((*position, self.payload));
        self.catalog.sequence = *position;
        self.catalog.pending_work += 1;
        Err(AppendFailure::Unknown("ack lost"))
    }

    fn reconcile(&mut self, _: &u64) -> Result<Option<u64>, Self::Error> {
        self.catalog
            .item
            .map(|(position, payload)| {
                reconcile_event(&intent(self.payload), &intent(payload), position)
                    .map_err(|EventConflict| "conflicting input")
            })
            .transpose()
    }

    fn proposed(&self, position: u64) -> u64 {
        position
    }
}

#[test]
fn lost_ack_joins_one_runnable_input_and_conflicting_reuse_fails() {
    let mut catalog = Catalog::default();
    let mut deliver = |payload, fail_before_write| {
        append(&mut Input {
            catalog: &mut catalog,
            payload,
            fail_before_write,
        })
    };
    assert_eq!(deliver("first", true), Err("disconnected"));
    assert_eq!(deliver("first", false), Ok(1));
    assert_eq!(deliver("first", false), Ok(1));
    assert_eq!(deliver("different", false), Err("conflicting input"));
    assert_eq!(catalog.sequence, 1);
    assert_eq!(catalog.pending_work, 1);
    assert_eq!(catalog.commits, 4);
}

#[test]
fn transactional_backends_retry_only_confirmed_conflicts() {
    let mut attempts = 0;
    let result = append_transaction(
        || {
            attempts += 1;
            match attempts {
                1 => Err(AppendFailure::Conflict("transaction rolled back")),
                2 => Err(AppendFailure::Unknown("ack lost")),
                _ => panic!("uncertain transaction was replayed"),
            }
        },
        || Ok::<Option<u64>, _>(None),
    );
    assert_eq!(result, Err("ack lost"));
    assert_eq!(attempts, 2);
    assert_eq!(
        append_transaction(|| Err(AppendFailure::Unknown("ack lost")), || Ok(Some(8))),
        Ok(8)
    );
    assert_eq!(
        append_transaction(
            || Err::<u64, _>(AppendFailure::Rejected("invalid")),
            || panic!("rejected operation reconciled")
        ),
        Err("invalid")
    );
}

#[test]
fn fence_retries_rebuild_ordered_fences_and_propagate_domain_conflicts() {
    let mut preparations = 0;
    let mut commits = 0;
    let result = commit_fenced(
        &["b-domain"],
        || {
            preparations += 1;
            Ok(vec![if preparations == 1 { "a-old" } else { "z-new" }])
        },
        |id| Ok((*id).to_string()),
        |operations| {
            commits += 1;
            match commits {
                1 => assert_eq!(operations, [&"a-old", &"b-domain"]),
                2 => assert_eq!(operations, [&"b-domain", &"z-new"]),
                _ => panic!("domain CAS failure was replayed"),
            }
            Err::<(), usize>(0)
        },
        |index| Ok(Some(*index)),
        || usize::MAX,
    );
    assert_eq!(result, Err(0));
    assert_eq!((preparations, commits), (2, 2));
}
