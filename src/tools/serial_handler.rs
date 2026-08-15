//! Serial MCP Handler using rust-sdk standard approach
//!
//! This implementation follows the official rust-sdk patterns for proper tool registration

use rmcp::{
    handler::server::{router::tool::ToolRouter, tool::Parameters},
    model::*,
    service::RequestContext,
    tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
};
use serde::Serialize;
use std::future::Future;
use std::sync::Arc;
use tracing::{debug, error, info};

use super::types::*;
use crate::automation::{
    plan_target, MacroExecutor, MacroPack, MacroPlan, MacroRegistry, RunReport,
    SerialMacroTransport, SimulatedMacroTransport,
};
use crate::broker::{BrokerConnectionManager, MAX_READ_BYTES};
use crate::config::Config;
use crate::events::{publish, SerialEvent};
use crate::serial::CaptureConfig;

/// Serial tool handler using rust-sdk standard patterns
#[derive(Clone)]
pub struct SerialHandler {
    connection_manager: Arc<BrokerConnectionManager>,
    macro_registry: Arc<MacroRegistry>,
    #[allow(dead_code)]
    config: Config,
    tool_router: ToolRouter<SerialHandler>,
}

#[tool_router]
impl SerialHandler {
    pub fn new(config: Config) -> Self {
        Self {
            connection_manager: Arc::new(BrokerConnectionManager::new()),
            macro_registry: Arc::new(MacroRegistry::default()),
            config,
            tool_router: Self::tool_router(),
        }
    }

    pub async fn macro_load_pack(
        &self,
        args: MacroLoadArgs,
    ) -> Result<crate::automation::MacroLoadRecord, McpError> {
        publish_tool_invocation("macro_load", None, None);
        let input = macro_pack_input(args)?;
        self.macro_registry.load_json(&input).map_err(mcp_error)
    }

    pub async fn macro_list_packs(
        &self,
        pack_id: Option<String>,
    ) -> Result<crate::automation::MacroList, McpError> {
        publish_tool_invocation("macro_list", None, None);
        self.macro_registry
            .list(pack_id.as_deref())
            .map_err(mcp_error)
    }

    pub async fn macro_unload_pack(&self, pack_id: &str) -> Result<MacroUnloadResponse, McpError> {
        publish_tool_invocation("macro_unload", None, None);
        let unloaded = self.macro_registry.unload(pack_id).map_err(mcp_error)?;
        Ok(MacroUnloadResponse {
            pack_id: pack_id.to_string(),
            unloaded,
        })
    }

    pub async fn macro_plan_pack(&self, args: MacroPlanArgs) -> Result<MacroPlan, McpError> {
        publish_tool_invocation("macro_plan", None, None);
        let target = args.target.into_target().map_err(mcp_error)?;
        match (args.pack_id, args.pack_json, args.path) {
            (Some(pack_id), None, None) => self
                .macro_registry
                .plan(&pack_id, target)
                .map_err(mcp_error),
            (None, Some(pack_json), None) => {
                let pack: MacroPack = serde_json::from_str(&pack_json).map_err(mcp_error)?;
                plan_target(&pack, target).map_err(mcp_error)
            }
            (None, None, Some(path)) => {
                let pack_json = std::fs::read_to_string(path).map_err(mcp_error)?;
                let pack: MacroPack = serde_json::from_str(&pack_json).map_err(mcp_error)?;
                plan_target(&pack, target).map_err(mcp_error)
            }
            _ => Err(mcp_error(
                "Specify exactly one of pack_id, pack_json, or path for macro_plan",
            )),
        }
    }

    pub async fn macro_run_loaded(&self, args: MacroRunArgs) -> Result<RunReport, McpError> {
        publish_tool_invocation("macro_run", None, None);
        let target = args.target.into_target().map_err(mcp_error)?;
        let plan = self
            .macro_registry
            .plan(&args.pack_id, target)
            .map_err(mcp_error)?;
        self.run_macro_plan(plan, args.input).await
    }

    pub async fn macro_run_inline_pack(
        &self,
        args: MacroRunInlineArgs,
    ) -> Result<RunReport, McpError> {
        publish_tool_invocation("macro_run_inline", None, None);
        let pack: MacroPack = serde_json::from_str(&args.pack_json).map_err(mcp_error)?;
        let target = args.target.into_target().map_err(mcp_error)?;
        let plan = plan_target(&pack, target).map_err(mcp_error)?;
        self.run_macro_plan(plan, args.input).await
    }

    async fn run_macro_plan(
        &self,
        plan: MacroPlan,
        input: MacroRunInput,
    ) -> Result<RunReport, McpError> {
        match input {
            MacroRunInput::Connection { connection_id } => {
                let connection = self
                    .connection_manager
                    .get(&connection_id)
                    .await
                    .map_err(mcp_error)?;
                MacroExecutor::real()
                    .run(plan, SerialMacroTransport::new(connection))
                    .await
                    .map_err(mcp_error)
            }
            MacroRunInput::Simulation { reads } => {
                let reads = reads
                    .iter()
                    .map(|chunk| chunk.as_bytes().to_vec())
                    .collect();
                MacroExecutor::simulated()
                    .run(plan, SimulatedMacroTransport::new(reads))
                    .await
                    .map_err(mcp_error)
            }
        }
    }

    #[tool(description = "Validate and load a JSON macro pack into the runtime registry")]
    async fn macro_load(
        &self,
        Parameters(args): Parameters<MacroLoadArgs>,
    ) -> Result<CallToolResult, McpError> {
        tool_json(&self.macro_load_pack(args).await?)
    }

    #[tool(description = "List loaded runtime macro packs")]
    async fn macro_list(
        &self,
        Parameters(args): Parameters<MacroListArgs>,
    ) -> Result<CallToolResult, McpError> {
        tool_json(&self.macro_list_packs(args.pack_id).await?)
    }

    #[tool(description = "Unload a runtime macro pack")]
    async fn macro_unload(
        &self,
        Parameters(args): Parameters<MacroUnloadArgs>,
    ) -> Result<CallToolResult, McpError> {
        tool_json(&self.macro_unload_pack(&args.pack_id).await?)
    }

    #[tool(description = "Expand a loaded macro or assembly without opening hardware")]
    async fn macro_plan(
        &self,
        Parameters(args): Parameters<MacroPlanArgs>,
    ) -> Result<CallToolResult, McpError> {
        tool_json(&self.macro_plan_pack(args).await?)
    }

    #[tool(
        description = "Run a loaded macro or assembly with an existing connection or simulation"
    )]
    async fn macro_run(
        &self,
        Parameters(args): Parameters<MacroRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        tool_json(&self.macro_run_loaded(args).await?)
    }

    #[tool(description = "Validate, plan, and run an inline macro pack without loading it")]
    async fn macro_run_inline(
        &self,
        Parameters(args): Parameters<MacroRunInlineArgs>,
    ) -> Result<CallToolResult, McpError> {
        tool_json(&self.macro_run_inline_pack(args).await?)
    }

    #[tool(description = "List all available serial ports on the system")]
    async fn list_ports(&self) -> Result<CallToolResult, McpError> {
        debug!("Listing available serial ports");
        publish_tool_invocation("list_ports", None, None);

        match self.connection_manager.list_ports().await {
            Ok(ports) => {
                info!("Found {} serial ports", ports.len());

                let message = if ports.is_empty() {
                    "No serial ports found on the system".to_string()
                } else {
                    let port_list = ports
                        .iter()
                        .map(|p| {
                            if let Some(ref hw_id) = p.hardware_id {
                                format!("- {}: {} ({})", p.name, p.description, hw_id)
                            } else {
                                format!("- {}: {}", p.name, p.description)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    format!("Found {} serial ports:\n{}", ports.len(), port_list)
                };

                Ok(CallToolResult::success(vec![Content::text(message)]))
            }
            Err(e) => {
                error!("Failed to list serial ports: {}", e);
                Err(McpError::internal_error(
                    format!("Failed to list ports: {}", e),
                    None,
                ))
            }
        }
    }

    #[tool(description = "List shared serial connections opened by GUI, MCP, or CLI")]
    async fn list_connections(&self) -> Result<CallToolResult, McpError> {
        publish_tool_invocation("list_connections", None, None);
        let connections = self.connection_manager.list().await.map_err(mcp_error)?;
        tool_json(&connections)
    }

    #[tool(description = "Open a serial port connection with specified configuration")]
    async fn open(
        &self,
        Parameters(args): Parameters<OpenArgs>,
    ) -> Result<CallToolResult, McpError> {
        debug!("Opening serial connection to {}", args.port);
        publish_tool_invocation("open", None, Some(&args.port));

        let config = crate::serial::ConnectionConfig::try_from(args).map_err(mcp_invalid_params)?;

        match self.connection_manager.open(config.clone()).await {
            Ok(connection_id) => {
                info!(
                    "Opened serial connection {} to {}",
                    connection_id, config.port
                );

                let message = format!(
                    "Serial connection opened\nConnection ID: {}\nPort: {}\nBaud rate: {}",
                    connection_id, config.port, config.baud_rate
                );

                Ok(CallToolResult::success(vec![Content::text(message)]))
            }
            Err(e) => {
                error!("Failed to open serial connection to {}: {}", config.port, e);
                let error_msg = format!("Error: Failed to open port {} - {}", config.port, e);
                Err(McpError::internal_error(error_msg, None))
            }
        }
    }

    #[tool(description = "Close an open serial port connection")]
    async fn close(
        &self,
        Parameters(args): Parameters<CloseArgs>,
    ) -> Result<CallToolResult, McpError> {
        debug!("Closing serial connection {}", args.connection_id);
        publish_tool_invocation("close", Some(&args.connection_id), None);

        match self.connection_manager.close(&args.connection_id).await {
            Ok(()) => {
                info!("Closed serial connection {}", args.connection_id);
                let message = format!(
                    "Serial connection closed\nConnection ID: {}",
                    args.connection_id
                );
                Ok(CallToolResult::success(vec![Content::text(message)]))
            }
            Err(e) => {
                error!("Failed to close connection {}: {}", args.connection_id, e);
                let error_msg = format!(
                    "Error: Failed to close connection {} - {}",
                    args.connection_id, e
                );
                Err(McpError::internal_error(error_msg, None))
            }
        }
    }

    #[tool(description = "Write data to a serial port connection")]
    async fn write(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, McpError> {
        debug!(
            "Writing to connection {} with encoding {}",
            args.connection_id, args.encoding
        );
        publish_tool_invocation("write", Some(&args.connection_id), None);

        let data = decode_data(&args.data, &args.encoding).map_err(mcp_invalid_params)?;

        // Get connection
        let connection = match self.connection_manager.get(&args.connection_id).await {
            Ok(conn) => conn,
            Err(e) => {
                error!("Invalid connection ID {}: {}", args.connection_id, e);
                let error_msg = format!("Error: Connection ID {} not found", args.connection_id);
                return Err(McpError::internal_error(error_msg, None));
            }
        };

        // Send data
        match connection.write(&data).await {
            Ok(bytes_written) => {
                debug!(
                    "Wrote {} bytes to connection {}",
                    bytes_written, args.connection_id
                );
                let message = format!(
                    "Data sent successfully\nConnection ID: {}\nBytes written: {}\nData: {:?}",
                    args.connection_id, bytes_written, args.data
                );
                Ok(CallToolResult::success(vec![Content::text(message)]))
            }
            Err(e) => {
                error!(
                    "Failed to write to connection {}: {}",
                    args.connection_id, e
                );
                let error_msg = format!("Error: Data sending failed - {}", e);
                Err(McpError::internal_error(error_msg, None))
            }
        }
    }

    #[tool(description = "Set RTS and/or DTR control line levels on a serial port connection")]
    async fn set_control_lines(
        &self,
        Parameters(args): Parameters<SetControlLinesArgs>,
    ) -> Result<CallToolResult, McpError> {
        debug!(
            "Setting control lines on connection {}: rts={:?} dtr={:?}",
            args.connection_id, args.rts, args.dtr
        );
        publish_tool_invocation("set_control_lines", Some(&args.connection_id), None);

        if !args.has_line_update() {
            return Err(mcp_invalid_params(
                "At least one of 'rts' or 'dtr' must be specified",
            ));
        }

        let connection = match self.connection_manager.get(&args.connection_id).await {
            Ok(conn) => conn,
            Err(e) => {
                error!("Invalid connection ID {}: {}", args.connection_id, e);
                return Err(McpError::internal_error(
                    format!("Error: Connection ID {} not found", args.connection_id),
                    None,
                ));
            }
        };

        if let Some(rts) = args.rts {
            if let Err(e) = connection.set_rts(rts).await {
                error!(
                    "Failed to set RTS on connection {}: {}",
                    args.connection_id, e
                );
                return Err(McpError::internal_error(
                    format!("Error: Failed to set RTS - {}", e),
                    None,
                ));
            }
        }

        if let Some(dtr) = args.dtr {
            if let Err(e) = connection.set_dtr(dtr).await {
                error!(
                    "Failed to set DTR on connection {}: {}",
                    args.connection_id, e
                );
                return Err(McpError::internal_error(
                    format!("Error: Failed to set DTR - {}", e),
                    None,
                ));
            }
        }

        let mut parts = vec![format!(
            "Control lines updated\nConnection ID: {}",
            args.connection_id
        )];
        if let Some(rts) = args.rts {
            parts.push(format!("RTS: {}", if rts { "high" } else { "low" }));
        }
        if let Some(dtr) = args.dtr {
            parts.push(format!("DTR: {}", if dtr { "high" } else { "low" }));
        }

        Ok(CallToolResult::success(vec![Content::text(
            parts.join("\n"),
        )]))
    }

    #[tool(description = "Read data from a serial port connection")]
    async fn read(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        debug!(
            "Reading from connection {} with timeout {:?}",
            args.connection_id, args.timeout_ms
        );
        publish_tool_invocation("read", Some(&args.connection_id), None);

        validate_encoding(&args.encoding)?;
        validate_max_bytes(args.max_bytes)?;
        let timeout_ms = args
            .timeout_ms
            .unwrap_or(self.config.serial.default_timeout_ms);
        let capture_config = args.duration_ms.map(|duration_ms| CaptureConfig {
            timeout_ms,
            max_bytes: args.max_bytes,
            duration_ms,
            start_trigger: args.start_trigger,
            initial_timeout_ms: args.initial_timeout_ms,
            idle_timeout_ms: args.idle_timeout_ms,
        });
        if let Some(config) = capture_config.as_ref() {
            config.validate().map_err(mcp_invalid_params)?;
        }

        // Get connection
        let connection = match self.connection_manager.get(&args.connection_id).await {
            Ok(conn) => conn,
            Err(e) => {
                error!("Invalid connection ID {}: {}", args.connection_id, e);
                let error_msg = format!("Error: Connection ID {} not found", args.connection_id);
                return Err(McpError::internal_error(error_msg, None));
            }
        };

        // Prepare buffer
        let mut buffer = vec![0u8; args.max_bytes];

        if let Some(capture_config) = capture_config {
            let report = connection
                .capture(capture_config.clone())
                .await
                .map_err(|e| {
                    error!(
                        "Failed to capture from connection {}: {}",
                        args.connection_id, e
                    );
                    McpError::internal_error(format!("Error: Data capture failed - {}", e), None)
                })?;
            let encoded = encode_data(&report.data, &args.encoding).map_err(|e| {
                error!("Failed to encode capture data: {}", e);
                McpError::internal_error(format!("Error: Data encoding failed - {}", e), None)
            })?;
            let response = ReadResponse {
                connection_id: args.connection_id,
                bytes_read: report.bytes_read(),
                data: encoded,
                encoding: args.encoding,
                status: if report.bytes_read() > 0 {
                    "ok".to_string()
                } else {
                    "timeout".to_string()
                },
                timeout_ms: Some(timeout_ms),
                duration_ms: Some(capture_config.duration_ms),
                start_trigger: Some(capture_config.start_trigger),
                initial_timeout_ms: capture_config.initial_timeout_ms,
                idle_timeout_ms: capture_config.idle_timeout_ms,
                waited_ms: Some(report.waited_ms),
                elapsed_ms: Some(report.elapsed_ms),
                completion_reason: Some(report.completion_reason),
                chunks: Some(report.chunks),
            };

            return tool_json(&response);
        }

        // Read data
        match connection.read(&mut buffer, Some(timeout_ms)).await {
            Ok(bytes_read) => {
                buffer.truncate(bytes_read);

                // Encode data
                match encode_data(&buffer, &args.encoding) {
                    Ok(encoded) => {
                        debug!(
                            "Read {} bytes from connection {}",
                            bytes_read, args.connection_id
                        );

                        let message = if bytes_read > 0 {
                            format!(
                                "Data read successfully\nConnection ID: {}\nBytes read: {}\nData: {:?}",
                                args.connection_id, bytes_read, encoded
                            )
                        } else {
                            format!(
                                "Read timeout\nConnection ID: {}\nTimeout: {}ms\nBytes read: 0",
                                args.connection_id, timeout_ms
                            )
                        };

                        Ok(CallToolResult::success(vec![Content::text(message)]))
                    }
                    Err(e) => {
                        error!("Failed to encode read data: {}", e);
                        let error_msg = format!("Error: Data encoding failed - {}", e);
                        Err(McpError::internal_error(error_msg, None))
                    }
                }
            }
            Err(e) => match e {
                crate::serial::LocalSerialError::ReadTimeout => {
                    debug!("Read timeout on connection {}", args.connection_id);
                    let message = format!(
                        "Read timeout\nConnection ID: {}\nTimeout: {}ms\nBytes read: 0",
                        args.connection_id, timeout_ms
                    );
                    Ok(CallToolResult::success(vec![Content::text(message)]))
                }
                _ => {
                    error!(
                        "Failed to read from connection {}: {}",
                        args.connection_id, e
                    );
                    let error_msg = format!("Error: Data reading failed - {}", e);
                    Err(McpError::internal_error(error_msg, None))
                }
            },
        }
    }
}

#[tool_handler]
impl ServerHandler for SerialHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some("A shared serial-port MCP server. Use list_connections first to discover ports already opened by the GUI or CLI. Use list_ports and open when a shared connection does not exist; opening the same port with identical settings returns the existing connection ID.".to_string()),
        }
    }

    async fn initialize(
        &self,
        _request: InitializeRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        info!("Serial MCP server initialized");
        Ok(self.get_info())
    }
}

fn macro_pack_input(args: MacroLoadArgs) -> Result<String, McpError> {
    match (args.pack_json, args.path) {
        (Some(input), None) => Ok(input),
        (None, Some(path)) => std::fs::read_to_string(path).map_err(mcp_error),
        _ => Err(mcp_error(
            "Specify exactly one of pack_json or path for macro_load",
        )),
    }
}

fn tool_json<T: Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    serde_json::to_string_pretty(value)
        .map(|json| CallToolResult::success(vec![Content::text(json)]))
        .map_err(mcp_error)
}

fn publish_tool_invocation(tool: &str, connection_id: Option<&str>, port: Option<&str>) {
    let mut event = SerialEvent::new("tool.invoked", format!("MCP tool invoked: {tool}"))
        .details(serde_json::json!({ "tool": tool }));
    if let Some(connection_id) = connection_id {
        event = event.connection(connection_id);
    }
    if let Some(port) = port {
        event = event.port(port);
    }
    publish(event);
}

fn mcp_error(error: impl std::fmt::Display) -> McpError {
    McpError::internal_error(error.to_string(), None)
}

fn mcp_invalid_params(error: impl std::fmt::Display) -> McpError {
    McpError::invalid_params(error.to_string(), None)
}

fn validate_max_bytes(max_bytes: usize) -> Result<(), McpError> {
    if max_bytes == 0 {
        return Err(mcp_invalid_params("max_bytes must be greater than zero"));
    }
    if max_bytes > MAX_READ_BYTES {
        return Err(mcp_invalid_params(format!(
            "max_bytes must not exceed {MAX_READ_BYTES}"
        )));
    }
    Ok(())
}

fn validate_encoding(encoding: &str) -> Result<(), McpError> {
    match encoding {
        "utf8" | "utf-8" | "hex" | "base64" => Ok(()),
        _ => Err(mcp_invalid_params(format!(
            "Unsupported encoding: {encoding}"
        ))),
    }
}

/// Decode data to bytes array
fn decode_data(data: &str, encoding: &str) -> Result<Vec<u8>, String> {
    match encoding {
        "utf8" | "utf-8" => Ok(data.as_bytes().to_vec()),
        "hex" => {
            let data = data.trim().replace(' ', "");
            if data.len() & 1 != 0 {
                return Err("Hex string must have even length".to_string());
            }
            if let Some(position) = data.bytes().position(|byte| !byte.is_ascii_hexdigit()) {
                return Err(format!("Invalid hex character at position {position}"));
            }

            (0..data.len())
                .step_by(2)
                .map(|i| {
                    u8::from_str_radix(&data[i..i + 2], 16)
                        .map_err(|_| format!("Invalid hex character at position {}", i))
                })
                .collect()
        }
        "base64" => {
            use base64::{engine::general_purpose, Engine as _};
            general_purpose::STANDARD
                .decode(data.trim())
                .map_err(|e| format!("Invalid base64: {}", e))
        }
        _ => Err(format!("Unsupported encoding: {}", encoding)),
    }
}

/// Encode bytes array to string
fn encode_data(data: &[u8], encoding: &str) -> Result<String, String> {
    match encoding {
        "utf8" | "utf-8" => {
            String::from_utf8(data.to_vec()).map_err(|e| format!("Invalid UTF-8: {}", e))
        }
        "hex" => Ok(data
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ")),
        "base64" => {
            use base64::{engine::general_purpose, Engine as _};
            Ok(general_purpose::STANDARD.encode(data))
        }
        _ => Err(format!("Unsupported encoding: {}", encoding)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serial::CaptureStartTrigger;

    fn assert_invalid_params(error: McpError, expected_message: &str) {
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(error.message.as_ref(), expected_message);
    }

    fn open_args() -> OpenArgs {
        OpenArgs {
            port: "TEST".to_string(),
            baud_rate: 19_200,
            data_bits: "8".to_string(),
            stop_bits: "1".to_string(),
            parity: "none".to_string(),
            flow_control: "none".to_string(),
        }
    }

    fn read_args(max_bytes: usize) -> ReadArgs {
        ReadArgs {
            connection_id: "missing-connection".to_string(),
            timeout_ms: None,
            max_bytes,
            encoding: "utf8".to_string(),
            duration_ms: None,
            start_trigger: CaptureStartTrigger::FirstByte,
            initial_timeout_ms: None,
            idle_timeout_ms: None,
        }
    }

    fn write_args(data: &str, encoding: &str) -> WriteArgs {
        WriteArgs {
            connection_id: "missing-connection".to_string(),
            data: data.to_string(),
            encoding: encoding.to_string(),
        }
    }

    #[test]
    fn maximum_max_bytes_is_accepted() {
        validate_max_bytes(MAX_READ_BYTES).expect("the documented limit must be accepted");
    }

    #[tokio::test]
    async fn invalid_open_settings_are_reported_as_invalid_params() {
        let handler = SerialHandler::new(Config::default());
        let mut args = open_args();
        args.parity = "mark".to_string();

        let error = handler
            .open(Parameters(args))
            .await
            .expect_err("invalid serial settings must be rejected before opening hardware");

        assert_invalid_params(error, "Unsupported parity value: mark");
    }

    #[tokio::test]
    async fn invalid_open_baud_rates_are_rejected_before_hardware_access() {
        let handler = SerialHandler::new(Config::default());

        for (baud_rate, expected_message) in [
            (0, "baud_rate must be between 1 and 4000000 (got 0)"),
            (
                4_000_001,
                "baud_rate must be between 1 and 4000000 (got 4000001)",
            ),
        ] {
            let mut args = open_args();
            args.baud_rate = baud_rate;

            let error = handler
                .open(Parameters(args))
                .await
                .expect_err("invalid baud rates must be rejected before opening hardware");

            assert_invalid_params(error, expected_message);
        }
    }

    #[tokio::test]
    async fn empty_open_port_is_rejected_before_hardware_access() {
        let handler = SerialHandler::new(Config::default());
        let mut args = open_args();
        args.port = "  ".to_string();

        let error = handler
            .open(Parameters(args))
            .await
            .expect_err("an empty port must be rejected before opening hardware");

        assert_invalid_params(error, "port must not be empty");
    }

    #[tokio::test]
    async fn invalid_max_bytes_is_rejected_before_connection_lookup() {
        let handler = SerialHandler::new(Config::default());

        for (max_bytes, expected_message) in [
            (0, "max_bytes must be greater than zero".to_string()),
            (
                MAX_READ_BYTES + 1,
                format!("max_bytes must not exceed {MAX_READ_BYTES}"),
            ),
        ] {
            let error = handler
                .read(Parameters(read_args(max_bytes)))
                .await
                .expect_err("invalid reads must be rejected before connection lookup");

            assert_invalid_params(error, &expected_message);
        }
    }

    #[tokio::test]
    async fn invalid_capture_settings_are_reported_as_invalid_params() {
        let handler = SerialHandler::new(Config::default());
        let mut args = read_args(1);
        args.duration_ms = Some(0);

        let error = handler
            .read(Parameters(args))
            .await
            .expect_err("invalid capture settings must be rejected before connection lookup");

        assert_invalid_params(
            error,
            "Invalid configuration: duration_ms must be greater than zero",
        );
    }

    #[tokio::test]
    async fn unsupported_read_encoding_is_rejected_before_connection_lookup() {
        let handler = SerialHandler::new(Config::default());
        let mut args = read_args(1);
        args.encoding = "binary".to_string();
        args.duration_ms = Some(100);

        let error = handler
            .read(Parameters(args))
            .await
            .expect_err("encoding must be validated before lookup or FIFO consumption");

        assert_invalid_params(error, "Unsupported encoding: binary");
    }

    #[tokio::test]
    async fn invalid_write_payloads_are_rejected_before_connection_lookup() {
        let handler = SerialHandler::new(Config::default());

        for (args, expected_message) in [
            (
                write_args("payload", "binary"),
                "Unsupported encoding: binary",
            ),
            (write_args("0", "hex"), "Hex string must have even length"),
            (
                write_args("GG", "hex"),
                "Invalid hex character at position 0",
            ),
            (
                write_args("😀", "hex"),
                "Invalid hex character at position 0",
            ),
        ] {
            let error = handler
                .write(Parameters(args))
                .await
                .expect_err("invalid payloads must be rejected before connection lookup");

            assert_invalid_params(error, expected_message);
        }

        let error = handler
            .write(Parameters(write_args("***", "base64")))
            .await
            .expect_err("invalid base64 must be rejected before connection lookup");
        assert_invalid_params(error, "Invalid base64: Invalid symbol 42, offset 0.");
    }

    #[tokio::test]
    async fn empty_control_line_update_is_rejected_before_connection_lookup() {
        let handler = SerialHandler::new(Config::default());
        let args = SetControlLinesArgs {
            connection_id: "missing-connection".to_string(),
            rts: None,
            dtr: None,
        };

        let error = handler
            .set_control_lines(Parameters(args))
            .await
            .expect_err("empty updates must be rejected before connection lookup");

        assert_invalid_params(error, "At least one of 'rts' or 'dtr' must be specified");
    }

    #[tokio::test]
    async fn connection_lookup_failures_remain_internal_errors() {
        let handler = SerialHandler::new(Config::default());

        let read_error = handler
            .read(Parameters(read_args(1)))
            .await
            .expect_err("a missing connection must fail at runtime");

        assert_eq!(read_error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(
            read_error.message.as_ref(),
            "Error: Connection ID missing-connection not found"
        );

        let write_error = handler
            .write(Parameters(write_args("valid", "utf8")))
            .await
            .expect_err("a missing connection must fail at runtime");
        assert_eq!(write_error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(
            write_error.message.as_ref(),
            "Error: Connection ID missing-connection not found"
        );

        let control_error = handler
            .set_control_lines(Parameters(SetControlLinesArgs {
                connection_id: "missing-connection".to_string(),
                rts: Some(true),
                dtr: None,
            }))
            .await
            .expect_err("a missing connection must fail at runtime");
        assert_eq!(control_error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(
            control_error.message.as_ref(),
            "Error: Connection ID missing-connection not found"
        );
    }
}
