use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    thread,
    time::Duration,
};

fn main() {
    let mut args = env::args().skip(1);
    let mode = args.next().unwrap();
    let receipt_log = args.next().unwrap();
    let invocation = args.next().unwrap();
    let claim = args.next().unwrap();
    let mut request = String::new();
    io::stdin().read_to_string(&mut request).unwrap();
    assert_eq!(fs::read_to_string(claim).unwrap(), request);
    let mut receipts = OpenOptions::new()
        .create(true)
        .append(true)
        .open(receipt_log)
        .unwrap();
    writeln!(receipts, "{invocation}").unwrap();
    match mode.as_str() {
        "mismatch" => println!(r#"{{"invocationId":"wrong","result":{{}}}}"#),
        "lost" => std::process::exit(3),
        "timeout" => thread::sleep(Duration::from_secs(30)),
        _ => println!(r#"{{"invocationId":"{invocation}","result":{{"request":{request}}}}}"#),
    }
}
