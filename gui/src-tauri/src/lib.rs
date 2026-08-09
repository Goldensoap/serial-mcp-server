use serde::{Deserialize, Serialize};
use serial_mcp_server::broker::{self, BrokerConnectionManager};
use serial_mcp_server::events::{self, SerialEvent, EVENT_ADDRESS_ENV, EVENT_SOURCE_ENV};
use serial_mcp_server::serial::{
    ConnectionConfig, DataBits, FlowControl, Parity, PortInfo, StopBits,
};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::Duration;
use tauri::{Emitter, State};

struct AppState {
    connections: Arc<BrokerConnectionManager>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenPortRequest {
    port: String,
    baud_rate: u32,
    data_bits: String,
    stop_bits: String,
    parity: String,
    flow_control: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenPortResponse {
    connection_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferResponse {
    bytes: usize,
    data: String,
    hex: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EventStreamInfo {
    address: String,
    log_path: String,
}

#[tauri::command]
async fn list_ports(state: State<'_, AppState>) -> Result<Vec<PortInfo>, String> {
    state
        .connections
        .list_ports()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn list_connections(
    state: State<'_, AppState>,
) -> Result<Vec<serial_mcp_server::serial::ConnectionStatus>, String> {
    state
        .connections
        .list()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn open_port(
    request: OpenPortRequest,
    state: State<'_, AppState>,
) -> Result<OpenPortResponse, String> {
    let config = ConnectionConfig {
        port: request.port,
        baud_rate: request.baud_rate,
        data_bits: parse_data_bits(&request.data_bits)?,
        stop_bits: parse_stop_bits(&request.stop_bits)?,
        parity: parse_parity(&request.parity)?,
        flow_control: parse_flow_control(&request.flow_control)?,
    };
    let connection_id = state
        .connections
        .open(config)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OpenPortResponse { connection_id })
}

#[tauri::command]
async fn close_port(connection_id: String, state: State<'_, AppState>) -> Result<(), String> {
    state
        .connections
        .close(&connection_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn write_serial(
    connection_id: String,
    data: String,
    encoding: String,
    state: State<'_, AppState>,
) -> Result<TransferResponse, String> {
    let payload = serial_mcp_server::tools::types::decode_data(&data, &encoding)?;
    let connection = state
        .connections
        .get(&connection_id)
        .await
        .map_err(|error| error.to_string())?;
    let bytes = connection
        .write(&payload)
        .await
        .map_err(|error| error.to_string())?;
    Ok(TransferResponse {
        bytes,
        data: String::from_utf8_lossy(&payload[..bytes]).into_owned(),
        hex: hex_view(&payload[..bytes]),
    })
}

#[tauri::command]
async fn read_serial(
    connection_id: String,
    timeout_ms: u64,
    max_bytes: usize,
    state: State<'_, AppState>,
) -> Result<TransferResponse, String> {
    let connection = state
        .connections
        .get(&connection_id)
        .await
        .map_err(|error| error.to_string())?;
    let mut payload = vec![0; max_bytes.clamp(1, 65_536)];
    let bytes = match connection.read(&mut payload, Some(timeout_ms)).await {
        Ok(bytes) => bytes,
        Err(serial_mcp_server::serial::LocalSerialError::ReadTimeout) => 0,
        Err(error) => return Err(error.to_string()),
    };
    payload.truncate(bytes);
    Ok(TransferResponse {
        bytes,
        data: String::from_utf8_lossy(&payload).into_owned(),
        hex: hex_view(&payload),
    })
}

#[tauri::command]
async fn set_control_lines(
    connection_id: String,
    rts: Option<bool>,
    dtr: Option<bool>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if rts.is_none() && dtr.is_none() {
        return Err("RTS or DTR must be provided".to_string());
    }
    let connection = state
        .connections
        .get(&connection_id)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(level) = rts {
        connection
            .set_rts(level)
            .await
            .map_err(|error| error.to_string())?;
    }
    if let Some(level) = dtr {
        connection
            .set_dtr(level)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn event_history(limit: usize) -> Result<Vec<SerialEvent>, String> {
    events::load_history(limit.clamp(1, 5_000)).map_err(|error| error.to_string())
}

#[tauri::command]
fn event_stream_info() -> EventStreamInfo {
    EventStreamInfo {
        address: events::event_address(),
        log_path: events::event_log_path().display().to_string(),
    }
}

fn parse_data_bits(value: &str) -> Result<DataBits, String> {
    match value {
        "5" => Ok(DataBits::Five),
        "6" => Ok(DataBits::Six),
        "7" => Ok(DataBits::Seven),
        "8" => Ok(DataBits::Eight),
        _ => Err(format!("Unsupported data bits: {value}")),
    }
}

fn parse_stop_bits(value: &str) -> Result<StopBits, String> {
    match value {
        "1" => Ok(StopBits::One),
        "2" => Ok(StopBits::Two),
        _ => Err(format!("Unsupported stop bits: {value}")),
    }
}

fn parse_parity(value: &str) -> Result<Parity, String> {
    match value {
        "none" => Ok(Parity::None),
        "odd" => Ok(Parity::Odd),
        "even" => Ok(Parity::Even),
        _ => Err(format!("Unsupported parity: {value}")),
    }
}

fn parse_flow_control(value: &str) -> Result<FlowControl, String> {
    match value {
        "none" => Ok(FlowControl::None),
        "software" => Ok(FlowControl::Software),
        "hardware" => Ok(FlowControl::Hardware),
        _ => Err(format!("Unsupported flow control: {value}")),
    }
}

fn hex_view(data: &[u8]) -> String {
    data.iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn start_udp_bridge(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let Ok(socket) = tokio::net::UdpSocket::bind(events::event_address()).await else {
            return;
        };
        let mut buffer = vec![0_u8; 65_535];
        loop {
            let Ok((length, _)) = socket.recv_from(&mut buffer).await else {
                break;
            };
            if let Ok(event) = serde_json::from_slice::<SerialEvent>(&buffer[..length]) {
                let _ = app.emit("serial-event", event);
            }
        }
    });
}

fn start_journal_bridge(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let path = events::event_log_path();
        let mut offset = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let mut carry = String::new();

        loop {
            std::thread::sleep(Duration::from_millis(250));
            let Ok(mut file) = std::fs::File::open(&path) else {
                continue;
            };
            let Ok(length) = file.metadata().map(|meta| meta.len()) else {
                continue;
            };
            if length < offset {
                offset = 0;
                carry.clear();
            }
            if length == offset || file.seek(SeekFrom::Start(offset)).is_err() {
                continue;
            }

            let mut chunk = String::new();
            if file.read_to_string(&mut chunk).is_err() {
                continue;
            }
            offset = length;
            carry.push_str(&chunk);

            while let Some(newline) = carry.find('\n') {
                let line = carry[..newline].trim_end_matches('\r').to_string();
                carry.drain(..=newline);
                if let Ok(event) = serde_json::from_str::<SerialEvent>(&line) {
                    let _ = app.emit("serial-event", event);
                }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    std::env::set_var(EVENT_SOURCE_ENV, "gui");
    // Preserve a caller-provided address while making the default explicit for
    // child processes launched from the desktop app.
    if std::env::var_os(EVENT_ADDRESS_ENV).is_none() {
        std::env::set_var(EVENT_ADDRESS_ENV, events::DEFAULT_EVENT_ADDRESS);
    }

    tauri::Builder::default()
        .manage(AppState {
            connections: Arc::new(BrokerConnectionManager::new()),
        })
        .setup(|app| {
            tauri::async_runtime::block_on(broker::ensure_broker())
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            start_udp_bridge(app.handle().clone());
            start_journal_bridge(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_ports,
            list_connections,
            open_port,
            close_port,
            write_serial,
            read_serial,
            set_control_lines,
            event_history,
            event_stream_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Serial MCP Console");
}
