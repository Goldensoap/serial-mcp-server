//! Local IPC broker that owns the physical serial connections.
//!
//! GUI, MCP, and CLI clients all talk to this broker.  The first long-lived
//! process (normally the Tauri GUI) hosts it; later processes reuse the same
//! connection IDs and never own or close the underlying port implicitly.

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::events::{self, publish, EventContext, SerialEvent};
use crate::serial::capture::{capture_with_reader, CaptureReader};
use crate::serial::{
    CaptureConfig, CaptureReport, ConnectionConfig, ConnectionManager, ConnectionStatus,
    LocalSerialError, PortInfo, SerialConnection,
};

pub const DEFAULT_BROKER_ADDRESS: &str = "127.0.0.1:47832";
pub const BROKER_ADDRESS_ENV: &str = "SERIAL_MCP_BROKER_ADDR";
const SHARED_RX_CAPACITY: usize = 1024 * 1024;

struct BrokerState {
    connections: ConnectionManager,
    receive_buffers: RwLock<HashMap<String, Arc<SharedReceiveBuffer>>>,
    open_lock: Mutex<()>,
}

impl BrokerState {
    fn new() -> Self {
        Self {
            connections: ConnectionManager::new(),
            receive_buffers: RwLock::new(HashMap::new()),
            open_lock: Mutex::new(()),
        }
    }

    async fn receive_buffer(
        &self,
        connection_id: &str,
    ) -> Result<Arc<SharedReceiveBuffer>, LocalSerialError> {
        self.receive_buffers
            .read()
            .await
            .get(connection_id)
            .cloned()
            .ok_or_else(|| LocalSerialError::InvalidConnection(connection_id.to_string()))
    }
}

struct SharedReceiveBuffer {
    data: Mutex<VecDeque<u8>>,
    notify: Notify,
    active: AtomicBool,
}

impl SharedReceiveBuffer {
    fn new() -> Self {
        Self {
            data: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            active: AtomicBool::new(true),
        }
    }

    async fn push(&self, bytes: &[u8]) {
        let mut data = self.data.lock().await;
        let overflow = data
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(SHARED_RX_CAPACITY);
        if overflow > 0 {
            let drain_length = overflow.min(data.len());
            data.drain(..drain_length);
        }
        data.extend(bytes);
        drop(data);
        self.notify.notify_waiters();
    }

    async fn read(&self, max_bytes: usize, timeout_ms: u64) -> Result<Vec<u8>, LocalSerialError> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let notified = self.notify.notified();
            {
                let mut data = self.data.lock().await;
                if !data.is_empty() {
                    let length = max_bytes.min(data.len());
                    return Ok(data.drain(..length).collect());
                }
            }

            if !self.active.load(Ordering::Acquire) {
                return Err(LocalSerialError::InvalidConnection(
                    "Shared connection is closed".to_string(),
                ));
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(LocalSerialError::ReadTimeout);
            }
        }
    }

    fn close(&self) {
        self.active.store(false, Ordering::Release);
        self.notify.notify_waiters();
    }
}

struct SharedBufferCaptureReader {
    buffer: Arc<SharedReceiveBuffer>,
}

#[async_trait]
impl CaptureReader for SharedBufferCaptureReader {
    async fn read_once(
        &mut self,
        buffer: &mut [u8],
        timeout_ms: u64,
    ) -> Result<usize, LocalSerialError> {
        let data = self.buffer.read(buffer.len(), timeout_ms).await?;
        buffer[..data.len()].copy_from_slice(&data);
        Ok(data.len())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BrokerEnvelope {
    source: String,
    process_id: u32,
    request: BrokerRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum BrokerRequest {
    Ping,
    ListPorts,
    ListConnections,
    Open {
        config: ConnectionConfig,
    },
    Close {
        connection_id: String,
    },
    Status {
        connection_id: String,
    },
    Write {
        connection_id: String,
        data: Vec<u8>,
    },
    Read {
        connection_id: String,
        timeout_ms: Option<u64>,
        max_bytes: usize,
    },
    Capture {
        connection_id: String,
        config: CaptureConfig,
    },
    SetControlLines {
        connection_id: String,
        rts: Option<bool>,
        dtr: Option<bool>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct BrokerResponse {
    ok: bool,
    #[serde(default)]
    result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<BrokerError>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BrokerError {
    code: String,
    message: String,
}

impl BrokerResponse {
    fn success<T: Serialize>(value: T) -> Self {
        match serde_json::to_value(value) {
            Ok(result) => Self {
                ok: true,
                result,
                error: None,
            },
            Err(error) => Self::failure("serialization", error.to_string()),
        }
    }

    fn failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: Value::Null,
            error: Some(BrokerError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }

    fn serial_error(error: LocalSerialError) -> Self {
        let code = match error {
            LocalSerialError::ReadTimeout => "read_timeout",
            LocalSerialError::InvalidConnection(_) => "invalid_connection",
            LocalSerialError::ConnectionExists(_) => "connection_exists",
            LocalSerialError::InvalidBaudRate(_) => "invalid_baud_rate",
            _ => "serial_error",
        };
        Self::failure(code, error.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct BrokerClient {
    address: String,
}

impl BrokerClient {
    pub fn new() -> Self {
        Self {
            address: broker_address(),
        }
    }

    pub fn at(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
        }
    }

    async fn call<T: DeserializeOwned>(
        &self,
        request: BrokerRequest,
    ) -> Result<T, LocalSerialError> {
        let mut stream = TcpStream::connect(&self.address)
            .await
            .map_err(|error| broker_io_error(&self.address, error))?;
        let envelope = BrokerEnvelope {
            source: events::event_source(),
            process_id: std::process::id(),
            request,
        };
        let mut payload = serde_json::to_vec(&envelope)
            .map_err(|error| LocalSerialError::InvalidConfig(error.to_string()))?;
        payload.push(b'\n');
        stream.write_all(&payload).await?;

        let mut response_line = String::new();
        BufReader::new(stream).read_line(&mut response_line).await?;
        let response: BrokerResponse = serde_json::from_str(&response_line)
            .map_err(|error| LocalSerialError::InvalidConfig(error.to_string()))?;
        if !response.ok {
            return Err(map_broker_error(response.error));
        }
        serde_json::from_value(response.result)
            .map_err(|error| LocalSerialError::InvalidConfig(error.to_string()))
    }

    pub async fn ping(&self) -> Result<(), LocalSerialError> {
        self.call(BrokerRequest::Ping).await
    }
}

impl Default for BrokerClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Default)]
pub struct BrokerConnectionManager {
    client: BrokerClient,
}

impl BrokerConnectionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn list_ports(&self) -> Result<Vec<PortInfo>, LocalSerialError> {
        self.client.call(BrokerRequest::ListPorts).await
    }

    pub async fn open(&self, config: ConnectionConfig) -> Result<String, LocalSerialError> {
        self.client.call(BrokerRequest::Open { config }).await
    }

    pub async fn close(&self, connection_id: &str) -> Result<(), LocalSerialError> {
        self.client
            .call(BrokerRequest::Close {
                connection_id: connection_id.to_string(),
            })
            .await
    }

    pub async fn get(
        &self,
        connection_id: &str,
    ) -> Result<Arc<BrokerSerialConnection>, LocalSerialError> {
        let status = self.status(connection_id).await?;
        Ok(Arc::new(BrokerSerialConnection {
            client: self.client.clone(),
            connection_id: status.id,
        }))
    }

    pub async fn status(&self, connection_id: &str) -> Result<ConnectionStatus, LocalSerialError> {
        self.client
            .call(BrokerRequest::Status {
                connection_id: connection_id.to_string(),
            })
            .await
    }

    pub async fn list(&self) -> Result<Vec<ConnectionStatus>, LocalSerialError> {
        self.client.call(BrokerRequest::ListConnections).await
    }
}

#[derive(Debug)]
pub struct BrokerSerialConnection {
    client: BrokerClient,
    connection_id: String,
}

impl BrokerSerialConnection {
    pub fn id(&self) -> &str {
        &self.connection_id
    }

    pub async fn write(&self, data: &[u8]) -> Result<usize, LocalSerialError> {
        self.client
            .call(BrokerRequest::Write {
                connection_id: self.connection_id.clone(),
                data: data.to_vec(),
            })
            .await
    }

    pub async fn read(
        &self,
        buffer: &mut [u8],
        timeout_ms: Option<u64>,
    ) -> Result<usize, LocalSerialError> {
        let data: Vec<u8> = self
            .client
            .call(BrokerRequest::Read {
                connection_id: self.connection_id.clone(),
                timeout_ms,
                max_bytes: buffer.len(),
            })
            .await?;
        let length = data.len().min(buffer.len());
        buffer[..length].copy_from_slice(&data[..length]);
        Ok(length)
    }

    pub async fn capture(&self, config: CaptureConfig) -> Result<CaptureReport, LocalSerialError> {
        self.client
            .call(BrokerRequest::Capture {
                connection_id: self.connection_id.clone(),
                config,
            })
            .await
    }

    pub async fn status(&self) -> Result<ConnectionStatus, LocalSerialError> {
        self.client
            .call(BrokerRequest::Status {
                connection_id: self.connection_id.clone(),
            })
            .await
    }

    pub async fn set_rts(&self, level: bool) -> Result<(), LocalSerialError> {
        self.set_control_lines(Some(level), None).await
    }

    pub async fn set_dtr(&self, level: bool) -> Result<(), LocalSerialError> {
        self.set_control_lines(None, Some(level)).await
    }

    async fn set_control_lines(
        &self,
        rts: Option<bool>,
        dtr: Option<bool>,
    ) -> Result<(), LocalSerialError> {
        self.client
            .call(BrokerRequest::SetControlLines {
                connection_id: self.connection_id.clone(),
                rts,
                dtr,
            })
            .await
    }
}

pub fn broker_address() -> String {
    std::env::var(BROKER_ADDRESS_ENV).unwrap_or_else(|_| DEFAULT_BROKER_ADDRESS.to_string())
}

/// Start the broker in this process if one is not already reachable.
///
/// Returns `true` when this process became the owner and `false` when another
/// process already owns the broker address.
pub async fn ensure_broker() -> Result<bool, LocalSerialError> {
    let client = BrokerClient::new();
    if client.ping().await.is_ok() {
        return Ok(false);
    }

    match TcpListener::bind(broker_address()).await {
        Ok(listener) => {
            tokio::spawn(run_broker(listener, Arc::new(BrokerState::new())));
            for _ in 0..20 {
                if client.ping().await.is_ok() {
                    return Ok(true);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(LocalSerialError::ConnectionFailed(
                "Local serial broker did not become ready".to_string(),
            ))
        }
        Err(bind_error) => {
            for _ in 0..20 {
                if client.ping().await.is_ok() {
                    return Ok(false);
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(broker_io_error(&broker_address(), bind_error))
        }
    }
}

async fn run_broker(listener: TcpListener, state: Arc<BrokerState>) {
    while let Ok((stream, _)) = listener.accept().await {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let _ = handle_client(stream, state).await;
        });
    }
}

async fn handle_client(stream: TcpStream, state: Arc<BrokerState>) -> std::io::Result<()> {
    let mut request_line = String::new();
    let mut reader = BufReader::new(stream);
    reader.read_line(&mut request_line).await?;
    let mut stream = reader.into_inner();

    let response = match serde_json::from_str::<BrokerEnvelope>(&request_line) {
        Ok(envelope) => {
            let context = EventContext {
                source: envelope.source,
                process_id: envelope.process_id,
            };
            events::with_event_context(context, handle_request(envelope.request, &state)).await
        }
        Err(error) => BrokerResponse::failure("invalid_request", error.to_string()),
    };

    let mut payload = serde_json::to_vec(&response)?;
    payload.push(b'\n');
    stream.write_all(&payload).await
}

async fn handle_request(request: BrokerRequest, state: &BrokerState) -> BrokerResponse {
    match request {
        BrokerRequest::Ping => BrokerResponse::success(()),
        BrokerRequest::ListPorts => match PortInfo::list_ports() {
            Ok(ports) => BrokerResponse::success(ports),
            Err(error) => BrokerResponse::failure("serial_error", error.to_string()),
        },
        BrokerRequest::ListConnections => BrokerResponse::success(state.connections.list().await),
        BrokerRequest::Open { config } => open_shared_connection(state, config).await,
        BrokerRequest::Close { connection_id } => {
            match state.connections.close(&connection_id).await {
                Ok(()) => {
                    if let Some(buffer) = state.receive_buffers.write().await.remove(&connection_id)
                    {
                        buffer.close();
                    }
                    BrokerResponse::success(())
                }
                Err(error) => BrokerResponse::serial_error(error),
            }
        }
        BrokerRequest::Status { connection_id } => {
            match state.connections.get(&connection_id).await {
                Ok(connection) => BrokerResponse::success(connection.status().await),
                Err(error) => BrokerResponse::serial_error(error),
            }
        }
        BrokerRequest::Write {
            connection_id,
            data,
        } => match state.connections.get(&connection_id).await {
            Ok(connection) => match connection.write(&data).await {
                Ok(bytes) => BrokerResponse::success(bytes),
                Err(error) => BrokerResponse::serial_error(error),
            },
            Err(error) => BrokerResponse::serial_error(error),
        },
        BrokerRequest::Read {
            connection_id,
            timeout_ms,
            max_bytes,
        } => match state.receive_buffer(&connection_id).await {
            Ok(buffer) => match buffer
                .read(max_bytes.clamp(1, 65_536), timeout_ms.unwrap_or(1_000))
                .await
            {
                Ok(data) => BrokerResponse::success(data),
                Err(error) => BrokerResponse::serial_error(error),
            },
            Err(error) => BrokerResponse::serial_error(error),
        },
        BrokerRequest::Capture {
            connection_id,
            config,
        } => match state.receive_buffer(&connection_id).await {
            Ok(buffer) => {
                let mut reader = SharedBufferCaptureReader { buffer };
                match capture_with_reader(&mut reader, config).await {
                    Ok(report) => BrokerResponse::success(report),
                    Err(error) => BrokerResponse::serial_error(error),
                }
            }
            Err(error) => BrokerResponse::serial_error(error),
        },
        BrokerRequest::SetControlLines {
            connection_id,
            rts,
            dtr,
        } => match state.connections.get(&connection_id).await {
            Ok(connection) => {
                if let Some(level) = rts {
                    if let Err(error) = connection.set_rts(level).await {
                        return BrokerResponse::serial_error(error);
                    }
                }
                if let Some(level) = dtr {
                    if let Err(error) = connection.set_dtr(level).await {
                        return BrokerResponse::serial_error(error);
                    }
                }
                BrokerResponse::success(())
            }
            Err(error) => BrokerResponse::serial_error(error),
        },
    }
}

async fn open_shared_connection(state: &BrokerState, config: ConnectionConfig) -> BrokerResponse {
    let _open_guard = state.open_lock.lock().await;
    for status in state.connections.list().await {
        if status.port == config.port {
            let matches = status.baud_rate == config.baud_rate
                && status.data_bits == config.data_bits
                && status.stop_bits == config.stop_bits
                && status.parity == config.parity
                && status.flow_control == config.flow_control;
            return if matches {
                BrokerResponse::success(status.id)
            } else {
                BrokerResponse::failure(
                    "connection_exists",
                    format!(
                        "Port {} is already open with different serial settings",
                        config.port
                    ),
                )
            };
        }
    }

    match state.connections.open(config).await {
        Ok(connection_id) => {
            let connection = match state.connections.get(&connection_id).await {
                Ok(connection) => connection,
                Err(error) => return BrokerResponse::serial_error(error),
            };
            let receive_buffer = Arc::new(SharedReceiveBuffer::new());
            state
                .receive_buffers
                .write()
                .await
                .insert(connection_id.clone(), Arc::clone(&receive_buffer));
            start_shared_reader(connection, receive_buffer);
            BrokerResponse::success(connection_id)
        }
        Err(error) => BrokerResponse::serial_error(error),
    }
}

fn start_shared_reader(
    connection: Arc<SerialConnection>,
    receive_buffer: Arc<SharedReceiveBuffer>,
) {
    let connection_id = connection.id().to_string();
    tokio::spawn(events::with_event_context(
        EventContext {
            source: "device".to_string(),
            process_id: std::process::id(),
        },
        async move {
            let status = connection.status().await;
            let mut buffer = vec![0_u8; 4096];
            while receive_buffer.active.load(Ordering::Acquire) {
                match connection.read_unobserved(&mut buffer, Some(100)).await {
                    Ok(bytes) if bytes > 0 => {
                        receive_buffer.push(&buffer[..bytes]).await;
                        publish(
                            SerialEvent::new(
                                "serial.rx",
                                format!("Received {bytes} bytes from device"),
                            )
                            .connection(connection_id.clone())
                            .port(status.port.clone())
                            .direction("rx")
                            .data(&buffer[..bytes]),
                        );
                    }
                    Ok(_) | Err(LocalSerialError::ReadTimeout) => {}
                    Err(_) => break,
                }
            }
        },
    ));
}

fn map_broker_error(error: Option<BrokerError>) -> LocalSerialError {
    let error = error.unwrap_or(BrokerError {
        code: "broker_error".to_string(),
        message: "Unknown broker error".to_string(),
    });
    match error.code.as_str() {
        "read_timeout" => LocalSerialError::ReadTimeout,
        "invalid_connection" => LocalSerialError::InvalidConnection(error.message),
        "connection_exists" => LocalSerialError::ConnectionExists(error.message),
        "invalid_baud_rate" => LocalSerialError::InvalidConfig(error.message),
        _ => LocalSerialError::InvalidConfig(error.message),
    }
}

fn broker_io_error(address: &str, error: std::io::Error) -> LocalSerialError {
    LocalSerialError::ConnectionFailed(format!(
        "Cannot reach shared serial broker at {address}: {error}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_protocol_round_trips_source_context() {
        let envelope = BrokerEnvelope {
            source: "mcp".to_string(),
            process_id: 42,
            request: BrokerRequest::Ping,
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let decoded: BrokerEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.source, "mcp");
        assert_eq!(decoded.process_id, 42);
        assert!(matches!(decoded.request, BrokerRequest::Ping));
    }

    #[tokio::test]
    async fn shared_receive_buffer_delivers_bytes_once_in_order() {
        let buffer = SharedReceiveBuffer::new();
        buffer.push(b"abcdef").await;

        assert_eq!(buffer.read(4, 10).await.unwrap(), b"abcd");
        assert_eq!(buffer.read(4, 10).await.unwrap(), b"ef");
        assert!(matches!(
            buffer.read(1, 1).await,
            Err(LocalSerialError::ReadTimeout)
        ));
    }

    #[tokio::test]
    async fn closing_shared_buffer_wakes_waiting_readers() {
        let buffer = Arc::new(SharedReceiveBuffer::new());
        let waiting = {
            let buffer = Arc::clone(&buffer);
            tokio::spawn(async move { buffer.read(10, 10_000).await })
        };
        tokio::task::yield_now().await;
        buffer.close();

        assert!(matches!(
            waiting.await.unwrap(),
            Err(LocalSerialError::InvalidConnection(_))
        ));
    }
}
