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
use tokio::sync::{Mutex, Notify, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use tokio::task::JoinHandle;

use crate::events::{self, publish, EventContext, SerialEvent};
use crate::serial::capture::{capture_with_reader, CaptureReader};
use crate::serial::{
    CaptureConfig, CaptureReport, ConnectionConfig, ConnectionManager, ConnectionStatus,
    LocalSerialError, PortInfo, SerialConnection,
};

pub const DEFAULT_BROKER_ADDRESS: &str = "127.0.0.1:47832";
pub const BROKER_ADDRESS_ENV: &str = "SERIAL_MCP_BROKER_ADDR";
pub const MAX_READ_BYTES: usize = 65_536;
const SHARED_RX_CAPACITY: usize = 1024 * 1024;
const CONNECTION_OPERATION_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const READER_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

fn max_bytes_error(max_bytes: usize) -> Option<&'static str> {
    if max_bytes == 0 {
        Some("max_bytes must be greater than zero")
    } else if max_bytes > MAX_READ_BYTES {
        Some("max_bytes must not exceed 65536")
    } else {
        None
    }
}

struct BrokerState {
    connections: ConnectionManager,
    receive_buffers: RwLock<HashMap<String, Arc<SharedReceiveBuffer>>>,
    reader_tasks: RwLock<HashMap<String, JoinHandle<()>>>,
    operation_gates: RwLock<HashMap<String, Arc<RwLock<ConnectionLifecycle>>>>,
    open_lock: Mutex<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionLifecycle {
    Open,
    Closed,
}

impl BrokerState {
    fn new() -> Self {
        Self {
            connections: ConnectionManager::new(),
            receive_buffers: RwLock::new(HashMap::new()),
            reader_tasks: RwLock::new(HashMap::new()),
            operation_gates: RwLock::new(HashMap::new()),
            open_lock: Mutex::new(()),
        }
    }

    async fn receive_buffer(
        &self,
        connection_id: &str,
    ) -> Result<Arc<SharedReceiveBuffer>, LocalSerialError> {
        if let Some(buffer) = self
            .receive_buffers
            .read()
            .await
            .get(connection_id)
            .cloned()
        {
            return Ok(buffer);
        }

        // An open request publishes the manager entry before it finishes
        // registering the shared buffer. Waiting for open/close serialization
        // removes that short initialization race before reporting an invalid ID.
        let _open_guard = self.open_lock.lock().await;
        self.receive_buffers
            .read()
            .await
            .get(connection_id)
            .cloned()
            .ok_or_else(|| LocalSerialError::InvalidConnection(connection_id.to_string()))
    }

    async fn operation_gate(
        &self,
        connection_id: &str,
    ) -> Result<Arc<RwLock<ConnectionLifecycle>>, LocalSerialError> {
        if let Some(gate) = self
            .operation_gates
            .read()
            .await
            .get(connection_id)
            .cloned()
        {
            return Ok(gate);
        }

        let _open_guard = self.open_lock.lock().await;
        self.operation_gates
            .read()
            .await
            .get(connection_id)
            .cloned()
            .ok_or_else(|| LocalSerialError::InvalidConnection(connection_id.to_string()))
    }

    async fn begin_operation(
        &self,
        connection_id: &str,
    ) -> Result<OwnedRwLockReadGuard<ConnectionLifecycle>, LocalSerialError> {
        begin_operation(self.operation_gate(connection_id).await?, connection_id).await
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

    async fn push(&self, bytes: &[u8]) -> bool {
        let mut data = self.data.lock().await;
        if !self.active.load(Ordering::Acquire) {
            return false;
        }
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
        true
    }

    async fn read(&self, max_bytes: usize, timeout_ms: u64) -> Result<Vec<u8>, LocalSerialError> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let notified = self.notify.notified();
            {
                let mut data = self.data.lock().await;
                if !self.active.load(Ordering::Acquire) {
                    return Err(LocalSerialError::InvalidConnection(
                        "Shared connection is closed".to_string(),
                    ));
                }
                if !data.is_empty() {
                    let length = max_bytes.min(data.len());
                    return Ok(data.drain(..length).collect());
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(LocalSerialError::ReadTimeout);
            }
        }
    }

    async fn close(&self) {
        let mut data = self.data.lock().await;
        self.active.store(false, Ordering::Release);
        data.clear();
        drop(data);
        self.notify.notify_waiters();
    }
}

async fn begin_operation(
    gate: Arc<RwLock<ConnectionLifecycle>>,
    connection_id: &str,
) -> Result<OwnedRwLockReadGuard<ConnectionLifecycle>, LocalSerialError> {
    let lifecycle = gate.read_owned().await;
    if *lifecycle == ConnectionLifecycle::Open {
        Ok(lifecycle)
    } else {
        Err(LocalSerialError::InvalidConnection(format!(
            "Connection {connection_id} is closing or closed"
        )))
    }
}

async fn begin_close(
    gate: Arc<RwLock<ConnectionLifecycle>>,
    timeout: Duration,
) -> Result<OwnedRwLockWriteGuard<ConnectionLifecycle>, ()> {
    tokio::time::timeout(timeout, gate.write_owned())
        .await
        .map_err(|_| ())
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
        BrokerRequest::ListConnections => {
            // SerialConnection publishes OPENED while open_shared_connection
            // still owns this lock. Waiting here makes that event and the
            // fully initialized buffer/gate/reader atomically discoverable.
            let _open_guard = state.open_lock.lock().await;
            BrokerResponse::success(state.connections.list().await)
        }
        BrokerRequest::Open { config } => open_shared_connection(state, config).await,
        BrokerRequest::Close { connection_id } => {
            let _open_guard = state.open_lock.lock().await;
            let operation_gate = match state
                .operation_gates
                .read()
                .await
                .get(&connection_id)
                .cloned()
            {
                Some(gate) => gate,
                None => {
                    return BrokerResponse::serial_error(LocalSerialError::InvalidConnection(
                        connection_id,
                    ))
                }
            };
            let mut lifecycle =
                match begin_close(operation_gate, CONNECTION_OPERATION_DRAIN_TIMEOUT).await {
                    Ok(lifecycle) => lifecycle,
                    Err(()) => {
                        let message = format!(
                        "Timed out waiting for in-flight operations on connection {connection_id}"
                    );
                        return BrokerResponse::failure("connection_busy", message);
                    }
                };
            if *lifecycle != ConnectionLifecycle::Open {
                return BrokerResponse::serial_error(LocalSerialError::InvalidConnection(
                    connection_id,
                ));
            }
            *lifecycle = ConnectionLifecycle::Closed;

            let receive_buffer = { state.receive_buffers.write().await.remove(&connection_id) };
            if let Some(buffer) = receive_buffer {
                buffer.close().await;
            }
            // The background reader takes a shared lifecycle guard for each
            // physical read. Release the exclusive guard after marking the
            // connection closed so it can observe that state and exit.
            drop(lifecycle);
            let reader_task = { state.reader_tasks.write().await.remove(&connection_id) };
            if let Some(reader_task) = reader_task {
                stop_reader_task(reader_task, READER_TASK_SHUTDOWN_TIMEOUT).await;
            }

            let response = match state.connections.close(&connection_id).await {
                Ok(()) => BrokerResponse::success(()),
                Err(error) => BrokerResponse::serial_error(error),
            };
            state.operation_gates.write().await.remove(&connection_id);
            response
        }
        BrokerRequest::Status { connection_id } => {
            let _operation = match state.begin_operation(&connection_id).await {
                Ok(operation) => operation,
                Err(error) => return BrokerResponse::serial_error(error),
            };
            match state.connections.get(&connection_id).await {
                Ok(connection) => BrokerResponse::success(connection.status().await),
                Err(error) => BrokerResponse::serial_error(error),
            }
        }
        BrokerRequest::Write {
            connection_id,
            data,
        } => {
            let _operation = match state.begin_operation(&connection_id).await {
                Ok(operation) => operation,
                Err(error) => return BrokerResponse::serial_error(error),
            };
            match state.connections.get(&connection_id).await {
                Ok(connection) => match connection.write(&data).await {
                    Ok(bytes) => BrokerResponse::success(bytes),
                    Err(error) => BrokerResponse::serial_error(error),
                },
                Err(error) => BrokerResponse::serial_error(error),
            }
        }
        BrokerRequest::Read {
            connection_id,
            timeout_ms,
            max_bytes,
        } => {
            if let Some(error) = max_bytes_error(max_bytes) {
                return BrokerResponse::failure("invalid_config", error);
            }
            match state.receive_buffer(&connection_id).await {
                Ok(buffer) => match buffer.read(max_bytes, timeout_ms.unwrap_or(1_000)).await {
                    Ok(data) => BrokerResponse::success(data),
                    Err(error) => BrokerResponse::serial_error(error),
                },
                Err(error) => BrokerResponse::serial_error(error),
            }
        }
        BrokerRequest::Capture {
            connection_id,
            config,
        } => {
            if let Some(error) = max_bytes_error(config.max_bytes) {
                return BrokerResponse::failure("invalid_config", error);
            }
            match state.receive_buffer(&connection_id).await {
                Ok(buffer) => {
                    let mut reader = SharedBufferCaptureReader { buffer };
                    match capture_with_reader(&mut reader, config).await {
                        Ok(report) => BrokerResponse::success(report),
                        Err(error) => BrokerResponse::serial_error(error),
                    }
                }
                Err(error) => BrokerResponse::serial_error(error),
            }
        }
        BrokerRequest::SetControlLines {
            connection_id,
            rts,
            dtr,
        } => {
            let _operation = match state.begin_operation(&connection_id).await {
                Ok(operation) => operation,
                Err(error) => return BrokerResponse::serial_error(error),
            };
            match state.connections.get(&connection_id).await {
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
            }
        }
    }
}

async fn stop_reader_task(mut reader_task: JoinHandle<()>, timeout: Duration) {
    if tokio::time::timeout(timeout, &mut reader_task)
        .await
        .is_err()
    {
        reader_task.abort();
        let _ = reader_task.await;
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
            let operation_gate = Arc::new(RwLock::new(ConnectionLifecycle::Open));
            state
                .operation_gates
                .write()
                .await
                .insert(connection_id.clone(), Arc::clone(&operation_gate));
            let reader_task = start_shared_reader(connection, receive_buffer, operation_gate);
            state
                .reader_tasks
                .write()
                .await
                .insert(connection_id.clone(), reader_task);
            BrokerResponse::success(connection_id)
        }
        Err(error) => BrokerResponse::serial_error(error),
    }
}

fn start_shared_reader(
    connection: Arc<SerialConnection>,
    receive_buffer: Arc<SharedReceiveBuffer>,
    operation_gate: Arc<RwLock<ConnectionLifecycle>>,
) -> JoinHandle<()> {
    let connection_id = connection.id().to_string();
    tokio::spawn(events::with_event_context(
        EventContext {
            source: "device".to_string(),
            process_id: std::process::id(),
        },
        async move {
            let status = connection.status().await;
            let mut buffer = vec![0_u8; 4096];
            loop {
                let _operation =
                    match begin_operation(Arc::clone(&operation_gate), &connection_id).await {
                        Ok(operation) => operation,
                        Err(_) => break,
                    };
                match connection.read_unobserved(&mut buffer, Some(100)).await {
                    Ok(bytes) if bytes > 0 => {
                        if receive_buffer.push(&buffer[..bytes]).await {
                            publish(
                                SerialEvent::new(
                                    "serial.rx",
                                    format!("Received {bytes} bytes from device"),
                                )
                                .connection(connection_id.clone())
                                .port(status.port.clone())
                                .direction("rx")
                                .data(&buffer[..bytes]),
                            )
                        }
                    }
                    Ok(_) | Err(LocalSerialError::ReadTimeout) => {}
                    Err(_) => break,
                }
            }
        },
    ))
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
        "connection_busy" => LocalSerialError::ConnectionFailed(error.message),
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

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

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
        buffer.close().await;

        assert!(matches!(
            waiting.await.unwrap(),
            Err(LocalSerialError::InvalidConnection(_))
        ));
    }

    #[tokio::test]
    async fn closing_shared_buffer_discards_unconsumed_bytes() {
        let buffer = SharedReceiveBuffer::new();
        assert!(buffer.push(b"stale").await);
        buffer.close().await;

        assert!(matches!(
            buffer.read(5, 10).await,
            Err(LocalSerialError::InvalidConnection(_))
        ));
    }

    #[tokio::test]
    async fn close_gate_waits_for_in_flight_operation_and_rejects_late_operations() {
        let gate = Arc::new(RwLock::new(ConnectionLifecycle::Open));
        let operation = begin_operation(Arc::clone(&gate), "connection")
            .await
            .unwrap();
        let close_started = Arc::new(Notify::new());
        let close_finished = Arc::new(AtomicBool::new(false));
        let closing = {
            let gate = Arc::clone(&gate);
            let close_started = Arc::clone(&close_started);
            let close_finished = Arc::clone(&close_finished);
            tokio::spawn(async move {
                close_started.notify_one();
                let mut lifecycle = begin_close(gate, Duration::from_secs(1))
                    .await
                    .expect("close should acquire the gate after the operation finishes");
                *lifecycle = ConnectionLifecycle::Closed;
                close_finished.store(true, Ordering::Release);
            })
        };
        close_started.notified().await;
        tokio::task::yield_now().await;
        assert!(!close_finished.load(Ordering::Acquire));

        drop(operation);
        closing.await.unwrap();
        assert!(close_finished.load(Ordering::Acquire));
        assert!(matches!(
            begin_operation(gate, "connection").await,
            Err(LocalSerialError::InvalidConnection(_))
        ));
    }

    #[tokio::test]
    async fn close_gate_timeout_preserves_open_connection_state() {
        let gate = Arc::new(RwLock::new(ConnectionLifecycle::Open));
        let operation = begin_operation(Arc::clone(&gate), "connection")
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            assert!(begin_close(Arc::clone(&gate), Duration::from_millis(10))
                .await
                .is_err());
        })
        .await
        .expect("a busy close must return within its deadline");

        drop(operation);
        begin_operation(gate, "connection")
            .await
            .expect("a timed-out close must leave the connection open");
    }

    #[test]
    fn broker_read_size_validation_enforces_protocol_bounds() {
        assert_eq!(
            max_bytes_error(0),
            Some("max_bytes must be greater than zero")
        );
        assert_eq!(max_bytes_error(1), None);
        assert_eq!(max_bytes_error(MAX_READ_BYTES), None);
        assert_eq!(
            max_bytes_error(MAX_READ_BYTES + 1),
            Some("max_bytes must not exceed 65536")
        );
    }

    #[tokio::test]
    async fn reader_task_shutdown_aborts_and_reaps_a_stuck_task() {
        let dropped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(Notify::new());
        let reader_task = {
            let dropped = Arc::clone(&dropped);
            let started = Arc::clone(&started);
            tokio::spawn(async move {
                let _drop_signal = DropSignal(dropped);
                started.notify_one();
                std::future::pending::<()>().await;
            })
        };
        started.notified().await;

        tokio::time::timeout(
            Duration::from_secs(1),
            stop_reader_task(reader_task, Duration::from_millis(10)),
        )
        .await
        .expect("stuck reader task should be aborted within the shutdown deadline");

        assert!(
            dropped.load(Ordering::Acquire),
            "aborted reader task should be awaited and dropped"
        );
    }
}
