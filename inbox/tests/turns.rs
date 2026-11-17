use syzygy_session_inbox::{TurnState, Wake};

#[test]
fn stale_wakes_stay_retired_after_the_recent_window_rolls_over() {
    let mut state = TurnState::default();
    assert!(matches!(state.schedule(Some("first"), 1), Wake::Start));
    let snapshot = state.generation();
    for position in 2..=300 {
        let identity = format!("input-{position}");
        assert!(matches!(
            state.schedule(Some(&identity), position),
            Wake::Coalesced
        ));
        state.observe([identity]);
    }
    assert_eq!(state.consume(snapshot, 1), 1);
    assert!(state.next_turn());
    assert_eq!(state.consume(state.generation(), 300), 300);
    assert!(!state.next_turn());
    assert!(!state.can_retire());
    state.acknowledge(300);
    assert!(state.can_retire());
    assert!(matches!(state.schedule(Some("first"), 1), Wake::Ignored));
    assert!(!state.busy());
}
