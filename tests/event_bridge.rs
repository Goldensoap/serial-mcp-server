use serial_mcp_server::broker::BROKER_ADDRESS_ENV;
use serial_mcp_server::events::{SerialEvent, EVENT_LOG_ENV};
use std::process::{Child, Command, Stdio};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn cli_process_publishes_events_for_gui_consumers() {
    let temp = tempfile::tempdir().expect("temporary event directory");
    let event_log = temp.path().join("events.jsonl");
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve broker port");
    let broker_address = reserved.local_addr().unwrap();
    drop(reserved);

    let output = Command::new(env!("CARGO_BIN_EXE_serial-mcp-server"))
        .args(["list-ports", "--json"])
        .env(EVENT_LOG_ENV, &event_log)
        .env(BROKER_ADDRESS_ENV, broker_address.to_string())
        .output()
        .expect("run the CLI in a separate process");

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let journal = std::fs::read_to_string(event_log).expect("cross-process event journal");
    let events = journal
        .lines()
        .map(|line| serde_json::from_str::<SerialEvent>(line).expect("valid event JSON"))
        .collect::<Vec<_>>();

    assert!(events
        .iter()
        .any(|event| { event.source == "cli" && event.event_type == "ports.scanned" }));
}

#[test]
fn second_process_reuses_long_lived_broker_and_preserves_its_source() {
    let temp = tempfile::tempdir().expect("temporary event directory");
    let event_log = temp.path().join("shared-events.jsonl");
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve broker port");
    let broker_address = reserved.local_addr().unwrap().to_string();
    drop(reserved);

    let owner = Command::new(env!("CARGO_BIN_EXE_serial-mcp-server"))
        .arg("serve")
        .env(BROKER_ADDRESS_ENV, &broker_address)
        .env(EVENT_LOG_ENV, &event_log)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start long-lived MCP broker owner");
    let _owner = ChildGuard(owner);

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let client = serial_mcp_server::broker::BrokerClient::at(&broker_address);
        for _ in 0..100 {
            if client.ping().await.is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("shared broker did not become ready");
    });

    let cli = Command::new(env!("CARGO_BIN_EXE_serial-mcp-server"))
        .args(["list-ports", "--json"])
        .env(BROKER_ADDRESS_ENV, &broker_address)
        .output()
        .expect("run a second CLI process against the existing broker");
    assert!(cli.status.success(), "second process failed");

    let journal = std::fs::read_to_string(event_log).expect("shared event journal");
    let events = journal
        .lines()
        .filter_map(|line| serde_json::from_str::<SerialEvent>(line).ok())
        .collect::<Vec<_>>();
    assert!(events
        .iter()
        .any(|event| event.source == "cli" && event.event_type == "ports.scanned"));
}
