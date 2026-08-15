# Serial MCP Console 使用说明

安装目录包含两个程序：

- `serial-mcp-console.exe`：桌面 GUI。启动后会运行本机串口共享代理。
- `serial-mcp-server.exe`：命令行工具，同时也是 MCP stdio 服务端。

## GUI

从开始菜单启动 **Serial MCP Console**。GUI、CLI 和 MCP 服务端通过本机共享代理访问同一串口；使用完全相同的串口参数打开同一端口时，会复用已有连接。

GUI 启动时不会自动启动 MCP stdio 服务。MCP 服务应由 Codex、Claude Desktop 等 MCP 客户端按需启动。

## CLI

MSI 安装会把安装目录加入系统 `PATH`。重新打开 PowerShell 后，可以直接运行：

```powershell
serial-mcp-server.exe list-ports --json
serial-mcp-server.exe probe --port COM3 --baud 115200 --json
serial-mcp-server.exe write --port COM3 --baud 115200 --data HELLO --read --json
serial-mcp-server.exe read --port COM3 --baud 115200 --timeout-ms 1000 --json
```

查看全部参数：

```powershell
serial-mcp-server.exe --help
```

## MCP 客户端配置

重启 MCP 客户端以读取更新后的系统 `PATH`，然后将 `command` 设置为
`serial-mcp-server.exe`，并添加 `serve` 参数。例如：

```json
{
  "mcpServers": {
    "serial": {
      "command": "serial-mcp-server.exe",
      "args": ["serve"]
    }
  }
}
```

也可以继续使用安装目录中的绝对路径。MCP 使用 stdio 通信，不应直接双击运行服务端。

## 共享代理与退出行为

- GUI、MCP 服务端或 CLI 中，最先启动且成功绑定代理地址的进程会成为共享代理所有者。
- GUI 正在运行时，CLI 和 MCP 服务端会复用 GUI 进程中的共享代理。
- MCP 服务端先启动时，随后启动的 GUI 也会复用 MCP 进程中的共享代理，并能访问同一批共享串口连接。
- GUI 和 MCP 服务端都未运行时，单次 CLI 命令会临时启动共享代理；命令结束后代理随 CLI 进程退出。
- 代理所有者退出后，代理及其持有的全部串口连接都会关闭。已经运行的 GUI 不会自动接管或重启代理，原连接状态也无法恢复；需要重启 GUI，或者启动一个持续运行的 MCP 服务端来重新建立代理。
- 为获得稳定的长生命周期代理，推荐先启动 GUI，再让 MCP 客户端按需启动 MCP 服务端。
- 默认代理地址为 `127.0.0.1:47832`。

## 硬件安全

RTS 和 DTR 可能连接到开发板的复位或启动电路。改变控制线电平前，请先确认目标硬件的接线和电气要求。
