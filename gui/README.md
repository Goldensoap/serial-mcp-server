# Serial MCP Console

这是 `serial-mcp-server` 的 Tauri 2 桌面端。它提供串口配置、共享连接列表、实时终端、MCP/CLI 活动视图、UTF-8/HEX 显示、RTS/DTR 控制和发送输入框。

## 核心架构

桌面端启动本机 TCP 代理（默认 `127.0.0.1:47832`）。代理独占真实串口；GUI、MCP stdio server 和 CLI 都是代理客户端。相同端口和相同参数的 `open` 是幂等操作，并返回同一个连接 ID。

代理同时生成跨进程事件：

- JSONL 日志：Windows 默认位于 `%LOCALAPPDATA%\serial-mcp-server\events.jsonl`。
- UDP 实时事件：默认发送到 `127.0.0.1:47831`。
- Tauri 后端同时监听 UDP 和日志增量，前端使用事件 ID 去重。

事件包含来源、客户端 PID、连接 ID、端口、方向、字节数、UTF-8 视图和 HEX 视图。设备 RX 由代理的单一后台读循环产生；GUI 不调用循环 `read`，因此不会消费掉 MCP/CLI 等待的数据。

## 开发运行

Windows 需要 Tauri 2 的常规前置依赖：Rust stable、Microsoft C++ Build Tools、Windows SDK 和 WebView2。然后运行：

```powershell
cd gui
npm install
npm run tauri dev
```

只验证前端：

```powershell
npm run build
```

检查 Tauri Rust 后端：

```powershell
cargo check --manifest-path src-tauri/Cargo.toml --locked
```

## 验收流程

1. 启动 GUI，在左侧选择串口和参数并点击“打开串口”。
2. 记下 GUI 活动连接中的端口和连接 ID。
3. 启动 MCP server：`serial-mcp-server serve`。
4. 从 MCP 调用 `list_connections`，应看到步骤 1 的同一个连接 ID。
5. 以相同参数调用 MCP `open`，返回值仍应是同一个连接 ID。
6. 调用 MCP `write`、`read` 或 `set_control_lines`。
7. GUI 终端应实时出现对应事件，来源标记为 `MCP`，PID 为 MCP server 进程；TX/RX 内容可在 UTF-8 和 HEX 间切换。
8. 运行 CLI，例如 `serial-mcp-server write --port COM3 --baud 115200 --data PING --json`；GUI 应实时显示 `CLI` 来源，且设备不会被第二次打开。
9. 结束 MCP/CLI 进程，GUI 中的共享连接应继续存在并可操作。
10. 在 GUI 中显式关闭连接，MCP `list_connections` 中该连接应消失。

没有真实串口时，可以运行仓库测试验证跨进程代理发现、来源透传和事件日志：

```powershell
cargo test --locked --all-targets --all-features
```

## 环境变量

| 变量 | 默认值 | 作用 |
| --- | --- | --- |
| `SERIAL_MCP_BROKER_ADDR` | `127.0.0.1:47832` | GUI/MCP/CLI 共享代理地址。 |
| `SERIAL_MCP_EVENT_ADDR` | `127.0.0.1:47831` | 实时 UDP 事件地址。 |
| `SERIAL_MCP_EVENT_LOG` | 平台本地应用数据目录 | 覆盖 JSONL 事件日志路径。 |
| `SERIAL_MCP_EVENT_SOURCE` | 由入口自动设置 | 事件来源；正常使用不需要手动设置。 |

代理只绑定 loopback，但没有额外鉴权。不要把 `SERIAL_MCP_BROKER_ADDR` 配置到非本机接口。
