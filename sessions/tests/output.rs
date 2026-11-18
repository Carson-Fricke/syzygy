use syzygy_runtime_contracts::OutputChannel::{Reasoning, Text};
use syzygy_session_policy::OutputBuffer;

#[test]
fn acknowledgement_advances_only_the_published_utf8_prefix() {
    let mut buffer = OutputBuffer::default();
    buffer.enqueue(Text, "α".into());
    let first = buffer.next_batch().unwrap();
    buffer.enqueue(Text, "β".into());
    buffer.enqueue(Reasoning, "next".into());
    let mut other = OutputBuffer::default();
    other.enqueue(Text, "α".into());
    assert!(!other.commit_batch(&first));
    assert!(buffer.commit_batch(&first));
    assert!(!buffer.commit_batch(&first));
    let mut second = buffer.next_batch().unwrap();
    assert_eq!((second.offset, second.text.as_str()), (2, "β"));
    second.text.clear();
    assert!(!buffer.commit_batch(&second));
    let second = buffer.next_batch().unwrap();
    assert!(buffer.commit_batch(&second));
    let third = buffer.next_batch().unwrap();
    assert_eq!((third.offset, third.channel), (0, Reasoning));
    assert!(buffer.commit_batch(&third));
    assert!(!buffer.has_pending());
}
