use serde_json::json;
use serial_mcp_server::events::{
    publish, SerialEvent, EVENT_ADDRESS_ENV, EVENT_LOG_ENV, EVENT_SOURCE_ENV,
};
use std::collections::HashSet;
use std::fs;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PROCESS_COUNT: usize = 8;
const EVENTS_PER_PROCESS: usize = 32;
const PAYLOAD_BYTES: usize = 16 * 1024;
const CHILD_MARKER_ENV: &str = "SERIAL_MCP_EVENT_JOURNAL_TEST_CHILD";
const CHILD_INDEX_ENV: &str = "SERIAL_MCP_EVENT_JOURNAL_TEST_CHILD_INDEX";
const BARRIER_DIR_ENV: &str = "SERIAL_MCP_EVENT_JOURNAL_TEST_BARRIER_DIR";
const CHILD_MARKER: &str = "concurrent-event-writer";
const CHILD_SOURCE: &str = "event-journal-concurrency-test";

#[test]
fn concurrent_event_writer_processes_append_complete_unique_json_lines() {
    if std::env::var(CHILD_MARKER_ENV).as_deref() == Ok(CHILD_MARKER) {
        run_event_writer_child();
        return;
    }

    let temp = tempfile::tempdir().expect("temporary event directory");
    let event_log = temp.path().join("concurrent-events.jsonl");
    let barrier_dir = temp.path().join("barrier");
    fs::create_dir(&barrier_dir).expect("create process barrier directory");

    let test_executable = std::env::current_exe().expect("current integration test executable");
    let test_name = "concurrent_event_writer_processes_append_complete_unique_json_lines";
    let mut children = Vec::with_capacity(PROCESS_COUNT);

    for process_index in 0..PROCESS_COUNT {
        children.push(
            Command::new(&test_executable)
                .args(["--exact", test_name, "--nocapture"])
                .env(CHILD_MARKER_ENV, CHILD_MARKER)
                .env(CHILD_INDEX_ENV, process_index.to_string())
                .env(BARRIER_DIR_ENV, &barrier_dir)
                .env(EVENT_LOG_ENV, &event_log)
                .env(EVENT_SOURCE_ENV, CHILD_SOURCE)
                .env(EVENT_ADDRESS_ENV, "127.0.0.1:9")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn concurrent event writer process"),
        );
    }

    let ready_deadline = Instant::now() + Duration::from_secs(15);
    let all_ready = loop {
        let ready_count = (0..PROCESS_COUNT)
            .filter(|index| barrier_dir.join(format!("ready-{index}")).exists())
            .count();
        if ready_count == PROCESS_COUNT {
            break true;
        }
        if Instant::now() >= ready_deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };

    // Release every child together. Releasing even after a timeout keeps failed
    // assertions from stranding child test processes at the barrier.
    fs::write(barrier_dir.join("release"), b"go").expect("release event writer processes");

    let mut child_failures = Vec::new();
    for (process_index, child) in children.into_iter().enumerate() {
        let output = child
            .wait_with_output()
            .expect("wait for event writer process");
        if !output.status.success() {
            child_failures.push(format!(
                "writer {process_index}: status={} stdout={} stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }

    assert!(
        all_ready,
        "not all {PROCESS_COUNT} event writers reached the start barrier"
    );
    assert!(
        child_failures.is_empty(),
        "event writer child failures:\n{}",
        child_failures.join("\n")
    );

    let journal = fs::read_to_string(&event_log).expect("concurrent event journal");
    let expected_event_count = PROCESS_COUNT * EVENTS_PER_PROCESS;
    assert_eq!(
        journal.bytes().filter(|byte| *byte == b'\n').count(),
        expected_event_count,
        "every event must end with exactly one newline"
    );

    let events = journal
        .lines()
        .enumerate()
        .map(|(line_number, line)| {
            serde_json::from_str::<SerialEvent>(line).unwrap_or_else(|error| {
                panic!(
                    "event journal line {} is not complete JSON: {error}",
                    line_number + 1
                )
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(
        events.len(),
        expected_event_count,
        "all child events must be present"
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        events.len(),
        "journal event IDs must remain unique"
    );

    let mut writer_sequences = HashSet::with_capacity(expected_event_count);
    let mut process_ids = HashSet::with_capacity(PROCESS_COUNT);
    for event in &events {
        assert_eq!(event.event_type, "journal.concurrent-write");
        assert_eq!(event.source, CHILD_SOURCE);
        process_ids.insert(event.process_id);

        let writer = event.details["writer"].as_u64().expect("writer index") as usize;
        let sequence = event.details["sequence"]
            .as_u64()
            .expect("writer event sequence") as usize;
        let payload = event.details["payload"].as_str().expect("long payload");

        assert!(writer < PROCESS_COUNT, "writer index is in range");
        assert!(sequence < EVENTS_PER_PROCESS, "event sequence is in range");
        assert_eq!(payload.len(), PAYLOAD_BYTES, "long payload is complete");
        assert!(
            writer_sequences.insert((writer, sequence)),
            "writer/sequence pairs must be unique"
        );
    }

    assert_eq!(writer_sequences.len(), expected_event_count);
    assert_eq!(
        process_ids.len(),
        PROCESS_COUNT,
        "events must come from all child processes"
    );
}

fn run_event_writer_child() {
    let process_index = std::env::var(CHILD_INDEX_ENV)
        .expect("child process index")
        .parse::<usize>()
        .expect("numeric child process index");
    let barrier_dir = std::path::PathBuf::from(
        std::env::var_os(BARRIER_DIR_ENV).expect("child process barrier directory"),
    );

    fs::write(
        barrier_dir.join(format!("ready-{process_index}")),
        std::process::id().to_string(),
    )
    .expect("signal child process readiness");

    let release = barrier_dir.join("release");
    let release_deadline = Instant::now() + Duration::from_secs(20);
    while !release.exists() {
        assert!(
            Instant::now() < release_deadline,
            "timed out waiting for parent process barrier release"
        );
        thread::sleep(Duration::from_millis(5));
    }

    for sequence in 0..EVENTS_PER_PROCESS {
        let prefix = format!("writer-{process_index:02}-sequence-{sequence:02}|");
        let payload = prefix.repeat(PAYLOAD_BYTES.div_ceil(prefix.len()));
        let payload = &payload[..PAYLOAD_BYTES];
        publish(
            SerialEvent::new(
                "journal.concurrent-write",
                format!("writer {process_index}, event {sequence}"),
            )
            .details(json!({
                "writer": process_index,
                "sequence": sequence,
                "payload": payload,
            })),
        );
    }
}
