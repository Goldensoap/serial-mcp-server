//! Cross-process event stream used by the MCP server, CLI, and desktop GUI.
//!
//! Every event is appended to a JSON Lines journal for durable history and is
//! also sent as a best-effort UDP datagram for low-latency GUI updates.  The
//! journal is the source of truth; consumers should de-duplicate by `id`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const EVENT_SCHEMA_VERSION: u8 = 1;
pub const DEFAULT_EVENT_ADDRESS: &str = "127.0.0.1:47831";
pub const EVENT_ADDRESS_ENV: &str = "SERIAL_MCP_EVENT_ADDR";
pub const EVENT_LOG_ENV: &str = "SERIAL_MCP_EVENT_LOG";
pub const EVENT_SOURCE_ENV: &str = "SERIAL_MCP_EVENT_SOURCE";

#[derive(Debug, Clone)]
pub struct EventContext {
    pub source: String,
    pub process_id: u32,
}

tokio::task_local! {
    static EVENT_CONTEXT: EventContext;
}

pub async fn with_event_context<F>(context: EventContext, future: F) -> F::Output
where
    F: std::future::Future,
{
    EVENT_CONTEXT.scope(context, future).await
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SerialEventData {
    pub bytes: usize,
    pub utf8: String,
    pub hex: String,
}

impl SerialEventData {
    pub fn from_bytes(data: &[u8]) -> Self {
        Self {
            bytes: data.len(),
            utf8: String::from_utf8_lossy(data).into_owned(),
            hex: data
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerialEvent {
    pub schema_version: u8,
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub process_id: u32,
    pub source: String,
    pub event_type: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<SerialEventData>,
    #[serde(default)]
    pub details: Value,
}

impl SerialEvent {
    pub fn new(event_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            process_id: event_process_id(),
            source: event_source(),
            event_type: event_type.into(),
            message: message.into(),
            connection_id: None,
            port: None,
            direction: None,
            data: None,
            details: json!({}),
        }
    }

    pub fn connection(mut self, connection_id: impl Into<String>) -> Self {
        self.connection_id = Some(connection_id.into());
        self
    }

    pub fn port(mut self, port: impl Into<String>) -> Self {
        self.port = Some(port.into());
        self
    }

    pub fn direction(mut self, direction: impl Into<String>) -> Self {
        self.direction = Some(direction.into());
        self
    }

    pub fn data(mut self, data: &[u8]) -> Self {
        self.data = Some(SerialEventData::from_bytes(data));
        self
    }

    pub fn details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

/// Publish an event without allowing telemetry failures to affect serial I/O.
pub fn publish(event: SerialEvent) {
    let Ok(serialized) = serde_json::to_vec(&event) else {
        return;
    };

    if let Some(parent) = event_log_path().parent() {
        let _ = fs::create_dir_all(parent);
    }

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(event_log_path())
    {
        let _ = file.write_all(&serialized);
        let _ = file.write_all(b"\n");
    }

    if let Ok(socket) = UdpSocket::bind("127.0.0.1:0") {
        let _ = socket.send_to(&serialized, event_address());
    }
}

pub fn event_address() -> String {
    std::env::var(EVENT_ADDRESS_ENV).unwrap_or_else(|_| DEFAULT_EVENT_ADDRESS.to_string())
}

pub fn event_source() -> String {
    EVENT_CONTEXT
        .try_with(|context| context.source.clone())
        .unwrap_or_else(|_| {
            std::env::var(EVENT_SOURCE_ENV).unwrap_or_else(|_| "unknown".to_string())
        })
}

pub fn event_process_id() -> u32 {
    EVENT_CONTEXT
        .try_with(|context| context.process_id)
        .unwrap_or_else(|_| std::process::id())
}

pub fn event_log_path() -> PathBuf {
    if let Some(path) = std::env::var_os(EVENT_LOG_ENV) {
        return PathBuf::from(path);
    }

    #[cfg(target_os = "windows")]
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(base)
            .join("serial-mcp-server")
            .join("events.jsonl");
    }

    #[cfg(not(target_os = "windows"))]
    if let Some(base) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(base)
            .join("serial-mcp-server")
            .join("events.jsonl");
    }

    std::env::temp_dir()
        .join("serial-mcp-server")
        .join("events.jsonl")
}

pub fn load_history(limit: usize) -> std::io::Result<Vec<SerialEvent>> {
    load_history_from(&event_log_path(), limit)
}

fn load_history_from(path: &Path, limit: usize) -> std::io::Result<Vec<SerialEvent>> {
    if limit == 0 || !path.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(path)?;
    let mut events = BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<SerialEvent>(&line).ok())
        .collect::<Vec<_>>();

    if events.len() > limit {
        events.drain(..events.len() - limit);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn event_data_preserves_text_and_hex_views() {
        let data = SerialEventData::from_bytes(&[b'H', b'i', 0xff]);
        assert_eq!(data.bytes, 3);
        assert_eq!(data.utf8, "Hi\u{fffd}");
        assert_eq!(data.hex, "48 69 ff");
    }

    #[test]
    fn history_skips_invalid_lines_and_returns_tail() {
        let mut file = NamedTempFile::new().expect("temporary event log");
        for index in 0..3 {
            let event = SerialEvent::new("test", format!("event {index}"));
            writeln!(file, "{}", serde_json::to_string(&event).unwrap()).unwrap();
        }
        writeln!(file, "not-json").unwrap();

        let events = load_history_from(file.path(), 2).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].message, "event 1");
        assert_eq!(events[1].message, "event 2");
    }
}
