# GUI、CLI、MCP 与共享串口代理硬件验收计划

## 1. 目标与固定环境

本计划用于验证新增 Tauri GUI 及其与 CLI、MCP stdio server 的集成是否符合 README 中描述的共享代理架构。

- 测试日期：2026-08-14 至 2026-08-15
- 操作系统：Windows（本机）
- 真实串口：`COM6`
- 串口参数：`19200 baud, 8 data bits, no parity, 1 stop bit, no flow control`（8N1）
- 对端行为：逐字节原样回显，不添加、删除或转换任何字节
- 默认 broker：`127.0.0.1:47832`
- 默认实时事件地址：`127.0.0.1:47831`
- 基线提交：`96adb9bd56a6021e82a509cc079f283ebf8f220d`（`main`）
- 验收版本：上述基线提交上的当前未提交补丁；本报告中的修复与验证结论不代表基线提交原样状态

只有真实命令成功访问 `COM6` 且收到符合预期的回显，才能标记为“真实硬件通过”。没有设备证据的测试只能标记为静态检查、模拟或自动化测试通过。

## 2. 通过标准

1. GUI 能发现并以 19200/8N1 打开 `COM6`，发送 UTF-8 与 HEX 数据，并实时显示准确的 TX/RX。
2. CLI 和 MCP 均能通过真实 `COM6` 完成无损回显；JSON/stdout 行为符合文档。
3. GUI、CLI、MCP 使用同一 localhost broker；相同串口与配置的 `open` 返回同一个 `connection_id`，不存在第二个物理端口句柄。
4. GUI 不轮询或消费共享 RX 缓冲；GUI 发出的字节可由 CLI/MCP 读取，同时 GUI 仍能通过事件流看到 RX。
5. CLI/MCP 的命令/工具调用、连接、TX 与控制线活动在 GUI 中自动出现并带请求方来源和 PID；物理 RX 按架构标记为 `device` 及 broker PID。正常情况下不依赖手动刷新，目标可见延迟不超过 1 秒。
6. CLI 或 MCP 客户端退出后，共享连接仍由 GUI/broker 持有；显式关闭后所有接口都能感知连接消失。
7. 所有仓库质量门通过。若发现缺陷，必须记录复现步骤、修复内容和回归证据。

## 3. 测试项

### A. 前置与构建

| ID | 测试项 | 操作 | 预期 |
| --- | --- | --- | --- |
| PRE-01 | 工作区与进程基线 | 记录 `git status`；确认测试前没有遗留 GUI/server 占用 `COM6` 或默认 broker 端口 | 已知、可复现的干净基线 |
| PRE-02 | 串口发现 | 使用 CLI `list-ports --json` | 合法 JSON，列表包含 `COM6` |
| BUILD-01 | Rust 格式 | `cargo fmt --all -- --check` | 退出码 0 |
| BUILD-02 | Rust lint | `cargo clippy --locked --all-targets --all-features -- -D warnings` | 退出码 0，无 warning |
| BUILD-03 | Rust 测试 | `cargo test --locked --all-targets --all-features` | 全部通过 |
| BUILD-04 | Rust 文档 | `cargo doc --locked --all-features --no-deps` | 退出码 0 |
| BUILD-05 | GUI 前端 | 在 `gui/` 执行 `npm run build` | TypeScript 与 Vite 构建通过 |
| BUILD-06 | Tauri 后端 | `cargo check --manifest-path gui/src-tauri/Cargo.toml --locked` | 退出码 0 |

### B. CLI 真实硬件

所有命令显式传入 `--port COM6 --baud 19200 --data-bits 8 --parity none --stop-bits 1 --flow-control none`。

| ID | 测试项 | 载荷/操作 | 预期 |
| --- | --- | --- | --- |
| CLI-01 | 端口探测 | `probe --json` | 成功打开并报告 `COM6`、19200；JSON 可解析 |
| CLI-02 | UTF-8 回显 | `write --data "CLI-ECHO-19200" --read --timeout-ms 1500 --json` | 写入数与读取数相等，返回文本逐字节一致 |
| CLI-03 | 二进制/HEX 回显 | `write --format hex --data "00 01 7f 80 ff 43 4c 49" --read --timeout-ms 1500 --json` | 返回 8 字节，HEX 为 `00017f80ff434c49` 或等价规范格式 |
| CLI-04 | 空闲读取超时 | 无待收数据时 `read --timeout-ms 300 --json` | 有界时间内返回 timeout/0 字节的文档化 JSON，而非挂死或污染 stdout |
| CLI-05 | stdout/stderr 契约 | 解析上述命令 stdout，并分别记录 stderr | stdout 只有命令数据/JSON，诊断不混入 stdout |
| CLI-06 | 控制线接口 | 设置 RTS/DTR 高、低并恢复；不对对端电平效果作断言 | 命令成功或给出明确驱动错误；事件流记录来源 `cli`，连接不中断 |

### C. MCP stdio 真实硬件

通过真实 stdio JSON-RPC 客户端驱动 `serial-mcp-server serve`，不以直接调用 Rust handler 代替协议测试。

| ID | 测试项 | 操作 | 预期 |
| --- | --- | --- | --- |
| MCP-01 | 协议握手与发现 | `initialize`、`notifications/initialized`、`tools/list` | 握手成功；至少发现 `list_ports`、`list_connections`、`open`、`write`、`read`、`close`、`set_control_lines` |
| MCP-02 | 串口与连接发现 | 调用 `list_ports`、`list_connections` | `COM6` 可见；GUI 已打开时能发现其连接 ID |
| MCP-03 | 幂等打开 | 对 GUI 已打开的 19200/8N1 `COM6` 调用 `open` | 返回与 GUI 完全相同的 `connection_id` |
| MCP-04 | UTF-8 回显 | `write` 发送唯一载荷，再 `read` | 真实回显逐字节一致；工具响应无协议错误 |
| MCP-05 | HEX 回显 | 发送包含 `00`、`80`、`ff` 的 HEX，再读取 HEX | 长度及所有字节一致 |
| MCP-06 | 空闲读取超时 | 无待收数据时有界 `read` | 文档化 timeout/空数据结果，server 保持可用 |
| MCP-07 | 客户端生命周期 | 结束 MCP stdio 进程后再次从 GUI/CLI 查询 | broker 中连接仍存在且可继续读写 |
| MCP-08 | 显式关闭 | 最终通过 MCP 或 GUI `close` | `list_connections` 不再返回该连接，GUI 同步为断开 |

### D. GUI 基本功能（真实窗口操作）

| ID | 测试项 | 操作 | 预期 |
| --- | --- | --- | --- |
| GUI-01 | 启动与事件桥 | 启动 Tauri GUI | 窗口正常渲染；事件桥在线并显示 UDP 地址与 journal 路径 |
| GUI-02 | 端口刷新 | 点击刷新并查看下拉框 | `COM6` 出现；界面不中断、不重复堆积异常项 |
| GUI-03 | 参数与打开 | 选择 COM6、19200、8、1、None 后打开 | 活动连接出现，显示 COM6/19200 和连接 ID；无错误 toast |
| GUI-04 | UTF-8 发送/回显 | 无行尾发送 `GUI-ECHO-19200` | 终端实时显示内容准确的 GUI TX 与设备 RX，字节数各为 14 |
| GUI-05 | HEX 发送/回显 | 选择 HEX，发送 `00 47 55 49 80 ff` | TX/RX 都准确显示 6 字节；HEX 视图保留 `00 47 55 49 80 ff` |
| GUI-05B | Base64 发送/回显 | 选择 Base64，发送 `AEH/` | 解码后的 `00 41 ff` 被逐字节发送、回显，并可在 HEX 视图核对 |
| GUI-06 | 行尾 | 分别选择 LF、CRLF、CR | RX 比正文分别多 1、2、1 字节，HEX 与选择一致 |
| GUI-07 | 终端视图 | 切换全部/RX/TX/活动和 UTF-8/HEX | 过滤正确；切换不丢失底层事件 |
| GUI-08 | 暂停与恢复 | 暂停显示，外部发送唯一载荷，再恢复 | 暂停时界面不重绘；恢复后积压事件完整出现 |
| GUI-09 | 自动滚动与清空 | 切换自动滚动；点击清空 | 控件状态可见；清空仅清当前前端视图，不关闭连接 |
| GUI-10 | 控制线 | 对选中连接操作 RTS/DTR 高低并恢复 | 操作不崩溃；成功时终端/活动区出现对应 GUI 事件 |
| GUI-11 | 关闭 | 点击连接的关闭控件 | 连接从列表消失，发送按钮不再能误用旧连接 |
| GUI-12 | 历史恢复 | 产生事件后重启 GUI | journal 尾部历史被载入且按事件 ID 去重，不出现 UDP+journal 双份记录 |

### E. 单一代理、并发操作与跨接口实时同步

此组测试必须保持 GUI 运行并由 GUI 首先打开 `COM6`。

| ID | 测试项 | 操作 | 预期 |
| --- | --- | --- | --- |
| ARCH-01 | 单连接 ID | 记录 GUI ID；MCP 同配置 `open`；CLI 同配置操作 | MCP 返回相同 ID；GUI 连接数始终为 1；CLI 不造成第二连接 |
| ARCH-02 | 配置冲突保护 | 共享连接存在时，外部接口尝试以不同波特率打开同一 `COM6` | 明确拒绝配置冲突，不创建第二物理句柄、不破坏原连接 |
| ARCH-03 | GUI 不抢 RX | GUI 发送唯一载荷但不主动 read，随后 MCP/CLI `read` | AI 接口读到完整回显；GUI 同时通过事件流看到 RX |
| ARCH-04 | CLI→GUI 实时同步 | CLI 执行唯一 UTF-8 与 HEX 回显命令 | GUI 在 1 秒内自动出现来源 `CLI` 的命令调用与 TX；回显 RX 来源为 `device`；PID 与载荷正确 |
| ARCH-05 | MCP→GUI 实时同步 | MCP 执行 list/open/write/read/control-line | GUI 在 1 秒内自动出现来源 `MCP` 的工具活动、TX 与控制线；回显 RX 来源为 `device`；PID 与载荷正确 |
| ARCH-06 | 三接口连续操作 | GUI、CLI、MCP 依次发送带唯一前缀的载荷，期间不关闭共享连接 | 三组字节都正确回显；GUI 事件来源可区分；统计不串线 |
| ARCH-07 | 客户端退出不释放 | 终止一次性 CLI 与 MCP stdio 客户端 | GUI 连接保持；GUI 随后仍能完成回显 |
| ARCH-08 | 显式关闭全局可见 | GUI 显式关闭，再由 MCP `list_connections`/CLI 操作检查 | 旧 ID 消失；后续操作需要重新打开并得到有效连接 |
| ARCH-09 | 事件去重和顺序 | 同时启用 UDP 与 journal bridge，检查唯一载荷事件 | 每个事件 ID 只显示一次；同一操作 TX 在 RX 前，字节统计正确 |

## 4. 执行顺序与隔离

1. 完成本文件并提交到工作区后，才开始执行测试命令。
2. 记录基线，构建 release/dev 所需产物并跑质量门。
3. 在 GUI 未运行时完成 CLI 独立硬件基线，然后确保没有残留 broker host。
4. 启动 GUI，由 GUI 打开 `COM6` 并保持运行。
5. 在同一 GUI 会话中执行 MCP 与 CLI 共享代理、实时事件及生命周期测试。
6. 最后测试显式关闭和 GUI 历史恢复。
7. 发现 bug 时先把复现证据写入第 5 节，做最小修复，运行相关自动化测试、完整质量门和受影响的真实硬件用例。

## 5. 结果记录

### 5.1 总体结论

本文件在任何测试命令执行前建立。随后在基线提交 `96adb9bd56a6021e82a509cc079f283ebf8f220d`（`main`）之上的当前未提交补丁中，完成真实 `COM6` 验收、缺陷修复、针对性自动化回归和最终质量门；所有计划内测试通过。真机与纯自动化证据分别记录如下，不以合成异常场景冒充硬件验证。

| 范围 | 状态 | 证据/备注 |
| --- | --- | --- |
| 前置与构建 | PASS | `list-ports --json` 发现 `COM6`（CH340）；最终 `cargo fmt --all -- --check`、clippy `-D warnings`、doc 均退出 0；Rust 测试 97/97（lib 74、main 9、event_bridge 3、event_journal_concurrency 1、macro_automation 10）；GUI `npm run build` 退出 0（1781 modules），Tauri `cargo check --manifest-path gui/src-tauri/Cargo.toml --locked` 退出 0 |
| CLI 真实硬件 | PASS（真实硬件） | `CLI-ECHO-19200` 回读 14/14 字节；`00 01 7f 80 ff 43 4c 49` 回读 8/8；空闲读取返回 timeout/0；正常 JSON stdout 无诊断混入；RTS/DTR 高低切换成功并恢复 |
| MCP stdio 真实硬件 | PASS（真实硬件） | 真实 NDJSON stdio 完成 initialize/tool discovery；GUI 已打开时复用完整 ID；UTF-8 14/14、二进制 8/8；控制线、超时、进程退出和显式 close 均通过；非法读取编码返回 `-32602` 后目标回显仍为 13/13，未被错误请求消费 |
| GUI 基本功能 | PASS（真实窗口/硬件） | Tauri 窗口发现并以 19200/8N1 打开 `COM6`；UTF-8、HEX、Base64、LF/CRLF/CR、过滤、暂停恢复、自动滚动、清空、RTS/DTR、关闭及历史恢复全部通过 |
| 单一代理与实时同步 | PASS（真实硬件） | GUI/MCP/CLI 复用一个 connection ID 和一条物理连接；115200 冲突被拒绝；CLI/MCP 事件均在 1 秒内自动出现；GUI 事件旁观不消费 AI 可读 RX |
| 缺陷修复回归 | PASS | 下表所列缺陷均已完成针对性回归；正常串口生命周期由真机验证，异常 pending reader、busy close 和边界输入由纯自动化验证；最终全量质量门通过 |

### 5.2 关键硬件与架构证据

- CLI 独立基线：19200/8N1 下 UTF-8 与含 `00/80/ff` 的 HEX 均逐字节回显；空闲读取在有界时间返回。
- GUI 首次打开共享连接后，MCP `open` 两次及 CLI `probe` 均复用同一完整 UUID；GUI 活动连接数保持 1。不同波特率 115200 的 CLI `probe` 明确返回“同一端口已以不同设置打开”，原连接继续工作。
- GUI 发送 `GUI-ECHO-19200` 后，GUI 先显示 GUI TX 与 DEVICE RX；随后 CLI `read` 仍读到完整 14 字节，证明 GUI 通过事件流感知而没有抢走共享 RX。
- 修复后 CLI `write` 的 `CLI-REG-001` 回读 11/11，GUI 依次显示 `command.invoked`、CLI TX、DEVICE RX；无数据的 CLI `read` 也会显示 `command.invoked`。
- MCP 回归时 `list_connections` 返回 GUI 卡片所示 UUID；`MCP-ZERO-001` 在一次被拒绝的 `max_bytes=0` 读取后仍完整回读 12/12 字节。MCP 退出后连接仍存在。
- MCP `close` 后紧接 `open` 在 161 ms 内完成并得到新 ID，`MCP-REOPEN` 回读 10/10；GUI 实时显示 MCP CLOSED/OPENED。MCP 退出后，人工点击该 MCP 创建的连接卡片，GUI 又成功发送并回显 `GUI-AFTER-REOPEN` 16/16 字节。
- 最终 GUI 真机回归：发送 `GUI-FINAL-ECHO` 并收到 14/14 字节回显；在 GUI 中正常关闭后，以新 UUID 重新打开 `COM6`，发送 `GUI-REOPEN-OK` 并收到 13/13 字节回显，随后再次正常关闭。
- 最后一轮共享代理真机冒烟：GUI 以完整 UUID `b8fbeccf-c51c-44f3-a5a3-bf87dbe625aa` 持有 `COM6`；CLI `probe` 返回同一 ID，`CLI-FINAL-LIVE` 回读 14/14；真实 MCP stdio 的 `list_connections`/`open` 也返回同一 ID，`MCP-FINAL-LIVE` 回读 14/14。GUI 在 1 秒内显示 CLI/MCP invocation、请求方 TX 与 DEVICE RX；MCP 退出后 GUI 连接仍为 1 且可用，最后由 GUI 正常关闭。
- GUI 图标重绘范围优化后再次启动最新源码，以 UUID `e15ac158-14df-47cb-b85f-ef6c9b38c4f4` 打开 `COM6`，发送 `GUI-AFTER-REVIEW` 并收到 16/16 字节回显；GUI 收/发累计均从 267 B 增至 283 B，随后正常关闭且活动连接数回到 0。
- 最终读取边界真机回归：真实 MCP 对 oversized `max_bytes` 明确拒绝且不消费串口数据，随后 `MCP-FINAL-BOUND` 仍完整回读 15/15 字节；CLI oversized `write --read` 返回失败、stdout 为空，并在打开或写入串口之前结束，未产生 TX。
- 最终 MCP 参数副作用真机回归：在 `COM6`/19200/8N1 写入 `MCP-ENC-GUARD` 13 字节；随后以非法 `encoding="binary"` 调用 read，收到 `invalid_params (-32602)`，再以 UTF-8 读取仍完整获得 13/13 字节、完成原因为 `max_bytes`，证明非法请求未消费共享 FIFO。
- 显式关闭后 MCP `list_connections` 返回空数组；最终进程审计未发现 `serial-mcp-console`、`serial-mcp-server`、Tauri dev 或 Vite 进程，Tauri dev 端口 `1420` 与 broker 地址 `127.0.0.1:47832` 均无监听。质量门结束后再次执行最新 `target/debug/serial-mcp-server.exe list-ports --json`，退出码为 0；共列出 5 个端口，`COM6` 仍为 `wch.cn USB-SERIAL CH340 (COM6)` 且 `available=true`，确认资源已释放。
- 最终事件日志审计以严格 UTF-8 读取 `%LOCALAPPDATA%\serial-mcp-server\events.jsonl`：文件 98088 bytes，共 333 行，333/333 均可解析且 `schema_version=1`，重复事件 ID 为 0。目标序列逐项核验：`MCP-FINAL-BOUND` 的 TX/RX 均为 15/15 字节并属于同一 `connection_id`；`GUI-FINAL-ECHO` 的 TX/RX 均为 14/14 字节并属于同一 `connection_id`；关闭并以新 UUID 重开后，`GUI-REOPEN-OK` 的 TX/RX 均为 13/13 字节并属于该新 `connection_id`；`GUI-AFTER-REVIEW` 的 TX/RX 均为 16/16 字节并属于同一 `connection_id`；`MCP-ENC-GUARD` 的 TX/RX 均为 13/13 字节并属于同一 `connection_id`。另有 8 个同步起跑的 event-writer 进程、合计 256 条长事件的自动化 journal 完整性测试通过。

### 5.3 发现并修复的缺陷

| ID | 复现/风险 | 修复 | 回归证据 |
| --- | --- | --- | --- |
| BUG-01 | CLI `read`、复用连接的 `probe` 等无数据事件操作不会在 GUI 显示 | CLI 每次分派前发布来源 `cli` 的 `command.invoked`，包含命令名和可用端口；不写 stdout | 跨进程自动测试覆盖成功/失败命令；真机空闲 `read` 在 GUI 实时可见 |
| BUG-02 | GUI 活动连接卡片不显示完整 `connection_id`，无法按 README 精确核对 | 卡片新增可选中复制、自动换行的完整 UUID | GUI 可见并读出完整 `a601d8df-9ccb-4b80-a519-4de19cfc9ba2`，与 MCP/CLI 返回一致 |
| BUG-03 | 显式 GUI close 的 `connection.closed` 被后台 reader 标记为 `device`；close 后立即 reopen 还有句柄释放竞态 | `connection.closed` 在 `SerialConnection` 最后一个引用进入 `Drop` 时发布；broker 使用 open/close 串行化及每连接生命周期门，先排空物理操作、关闭并清空 RX buffer、让 reader 退出并 join，再从 manager 移除连接。broker 持有 `open_lock` 直到该引用的 `Drop` 与字段析构完成、close 即将返回，因此 CLOSED 继承请求方上下文，后续 broker reopen 也不会越过旧物理句柄的析构 | GUI close 显示来源 GUI；MCP close 显示来源 MCP；既有 `MCP-REOPEN` 真机回归成功，最终 GUI 又以新 UUID 重开并完成 `GUI-REOPEN-OK` 13/13 后正常关闭 |
| BUG-04 | JSONL 的 JSON 与换行分两次跨进程追加，存在交错坏行风险 | 每个事件改为带换行的一次追加写 | 8 个独立 event-writer 进程经屏障同步起跑，每进程追加 32 条、每条含 16 KiB 载荷；256/256 行完整可解析、writer/sequence 与事件 ID 唯一；最终真实运行 journal 也全部合法且无重复 ID |
| BUG-05 | 单次 `max_bytes=0` 会被 broker 夹成 1，消费一个字节但向调用方返回 0 | CLI、MCP 与 broker 均明确拒绝零长度读取，CLI 在打开/写入前校验 | CLI 返回明确错误且没有写串口；MCP 拒绝后 `MCP-ZERO-001` 仍完整回读 12 字节 |
| BUG-06 | MCP `open` 对非法串口枚举值静默回退到默认值 | `OpenArgs` 改为严格 `TryFrom` 校验 | `data_bits="9"` 返回协议错误，既有 COM6 连接 ID 和数量不变 |
| BUG-07 | MCP 单次 read 未使用配置中的默认 timeout | 单次与采集读取统一采用配置默认 timeout | Rust 全量测试与 MCP 真实超时/采集回归通过 |
| BUG-08 | GUI 动态重绘只向 Lucide 提供局部图标集，开发控制台反复报告缺失图标 | 所有初始和动态重绘统一使用完整图标集 | 启动、历史加载、暂停/恢复和自动滚动重绘后缺失图标 warning 为 0 |
| BUG-09 | broker close 若直接无限等待后台 reader，reader 卡在 pending I/O 或锁竞争时会让 close 及后续 open/close 永久挂起 | reader join 改为有界等待；超过 shutdown deadline 后执行 `abort` 并继续 `await` 回收任务，确保其持有的连接引用被释放 | 异常 pending reader 仅用自动化任务验证 timeout、abort、reap 与捕获资源 Drop；正常关闭/重开由最终 GUI 真机流程验证，不把合成 pending 场景冒充硬件证据 |
| BUG-10 | `max_bytes=0` 会消费隐藏字节，过大的 `max_bytes` 又可在 broker 前触发大内存分配；同类风险覆盖 direct read、capture 与 macro `expect` | 将有效范围统一为 `1..=65536`，在 broker 协议层、CLI/MCP 入口层以及 macro DSL/规划/真实 transport 层分配前校验；schema、CLI help、README 与 skill 参考同步 | 自动化覆盖 `0`、`1`、`65536`、`65537` 及 CLI read/write--read、MCP、broker、macro 入口；真机 MCP oversized 拒绝后 `MCP-FINAL-BOUND` 15/15，CLI oversized write--read stdout 为空且未写串口 |
| BUG-11 | close 与并发 write/status/control/物理 reader 竞态时，可能提前返回、提前发布 CLOSED，或让旧引用继续持有句柄；busy close 若先改状态会破坏仍可用连接 | 每个连接新增生命周期 `RwLock`：物理操作与 reader 持共享 guard，close 在有限时间内获取排他 guard；超时不改变 Open 状态，成功后标记 Closed、清空/关闭 buffer、停止 reader、移除 manager，最后由 Drop 发布 CLOSED | busy/timeout/late-operation/buffer-clear 异常路径仅由自动化验证；正常 GUI 真机执行 `GUI-FINAL-ECHO` 14/14、关闭、新 UUID 重开、`GUI-REOPEN-OK` 13/13、再关闭 |
| BUG-12 | CLI `write` 先打开/复用串口再解码 HEX/Base64；非法载荷可在长生命周期 broker 中留下调用方未获得 ID 的共享连接 | 将 payload 解码移动到 `open_connection` 之前；既有 capture 边界校验仍先于端口副作用 | 缺失测试端口配合非法 HEX、Base64 均返回 encoding error，而不是端口打开错误，证明解码先于打开 |
| BUG-13 | MCP 的非法串口配置、编码/载荷、`max_bytes`、capture 参数与空 control 更新被映射为 `internal_error (-32603)`；空端口和越界波特率也缺少完整入口校验 | 用户输入校验统一映射为 `invalid_params (-32602)`，覆盖 port 非空、baud `1..=4000000`、串口枚举、HEX/Base64/读取编码、读取边界、capture 与 control 更新；连接查找/串口 I/O 等运行时错误继续为 `-32603`。`tool.invoked` 仍先发布，故无效 AI 调用也会在 GUI 实时显示，但校验先于 broker/FIFO/硬件副作用 | handler 测试同时断言错误码、消息与校验顺序，并覆盖用户输入及运行时边界；代码路径确认 invocation 先于校验，真实 MCP 则确认非法 read 编码未消费目标回显 |
| BUG-14 | GUI 每个实时事件会对动态连接区和活动区重复执行全页 Lucide 图标扫描/替换，高频 RX 下造成无谓 DOM churn | 初始页面只全局初始化一次；动态重绘把 `createIcons` 的 root 限定为刚更新的连接列表、活动 feed 或按钮 | TypeScript/Vite 构建通过；修复后最新 Tauri GUI 真机完成 `GUI-AFTER-REVIEW` 16/16 且 TX/RX 实时显示、图标正常 |
| BUG-15 | MCP read 在校验输出编码前可能先消费破坏性共享 FIFO；write 也会在解码非法载荷前查找连接 | read 编码与 capture 配置在 FIFO 操作前校验；write 在连接查找/串口写入前解码，并对非 ASCII HEX 做安全检查以避免字节边界切片 panic | 单元测试用缺失连接证明参数错误优先；真机 `MCP-ENC-GUARD` 的非法编码读取返回 `-32602` 后，合法读取仍完整回显 13/13 |
| BUG-16 | 库单元测试直接调用 handler 时会把 `tool.invoked` 写入用户默认 JSONL/UDP；macro 集成测试的直接 handler 调用与 CLI 子进程也会污染真实 GUI 历史/实时流 | `cfg(test)` 的库内 publisher 使用无 I/O sink；产品构建与集成测试仍走真实 publisher；macro 集成测试把直接调用和子进程都导向临时 JSONL及丢弃 UDP 地址，event_bridge 子进程同样隔离默认 UDP | 集成测试通过生产 publisher 在临时目标验证 JSONL，UDP 指向隔离地址；GUI 的实时 UDP 与 journal 补偿链路由既有真机同步流程验证。最终 v4 全量门前后默认 journal 均为 331 行、97561 bytes、mtime `2026-08-15T15:23:34.2052925+08:00`，新增事件为 0 |

### 5.4 自动化防御矩阵

下表只证明异常控制流和输入边界，不声称这些合成场景访问过真实 `COM6`。真实串口证据只采用第 5.2 节明确标注的 GUI、CLI 与 MCP 操作。

| 防御项 | 自动化输入/场景 | 断言 |
| --- | --- | --- |
| pending reader 回收 | reader task 永久 pending；使用很短 shutdown deadline | join 有界返回；超时后任务被 abort 并 reap；任务捕获资源发生 Drop |
| lifecycle 正常排空 | operation 持共享 guard，close 请求排他 guard | close 在 operation 结束前不完成；成功标记 Closed；后到 operation 被拒绝 |
| lifecycle busy timeout | operation 持续占用超过 close deadline | close 有界返回 busy；生命周期仍为 Open；原连接、buffer、reader 与 manager 不被提前拆除，随后操作仍可进入 |
| close 清 buffer | buffer 中预置未消费字节并执行 close | 等待 reader 被唤醒；旧 buffer 变为 inactive；未消费字节被清除且不会在重开后泄漏 |
| 读取下界 | `max_bytes=0` | broker、CLI、MCP 与 macro 入口均明确拒绝；不会夹成 1 或消费串口字节 |
| 最小合法值 | `max_bytes=1` | broker 与 macro 边界校验接受，不发生零长度特殊路径 |
| 最大合法值 | `max_bytes=65536` | broker、CLI、MCP schema/handler 与 macro 边界校验接受，分配保持在统一上限内 |
| 超上界 | `max_bytes=65537` | broker、CLI read、CLI write--read、MCP direct/capture 与 macro `expect` 在分配或串口副作用前拒绝 |
| CLI 非法编码载荷 | 非法 HEX 与 Base64，端口名同时设为不存在 | 返回 encoding error，证明 payload 在打开/复用端口前完成解码 |
| MCP 参数错误分类 | 空/纯空白 port、baud 边界、非法 open 枚举、write HEX/Base64/编码、read 编码/`max_bytes`、capture duration、空 control 更新与缺失连接 ID | 用户参数错误为 `-32602`；无效调用仍发布 invocation，但连接/FIFO/串口不发生副作用；运行时连接错误仍为 `-32603` |
| MCP 非法读取不消费 | 先准备目标数据，再提交非法 read 编码，最后以合法编码读取 | 非法调用为 `-32602`，合法调用仍取得全部目标字节；真机另以 `MCP-ENC-GUARD` 13/13 验证 |
| 自动化事件隔离 | 库单元测试调用 MCP handler；macro 集成测试直接调用 handler 并启动 CLI 子进程；event_bridge 启动产品子进程 | 单元测试 publisher 不做真实 I/O；集成测试通过生产 publisher 写临时 JSONL并把 UDP 指向丢弃地址，不触碰用户默认事件流；GUI UDP 实时显示另由真机验收覆盖 |

### 5.5 架构语义说明

物理 RX 只由 broker 读取一次。GUI 是事件观察者，通过事件流实时感知 TX/RX，但不持有读取游标、也不消费 RX；MCP 和 CLI reader 共享同一个有界、破坏性单 FIFO，一方读走的字节不会重放给另一方。因此“一个人使用 GUI + 一个通过 MCP 或 CLI 的 AI 同时操作和感知”已经验证通过，但两个独立 AI reader 并不是广播订阅者，不能各自获得同一批 RX。若未来需要双 AI 广播读取，必须另行设计按订阅者维护的读取游标；这不是当前 README 声明的语义，也不是本轮通过标准。

## 6. 硬件与操作安全

- RTS/DTR 可能连接复位或启动电路。这里只做短暂高/低切换并恢复，不据此声称目标板电平功能正确；若设备断连，立即停止该组测试。
- 每个写入使用唯一、短小载荷，避免把历史缓冲误判为本次回显。
- 不以进程强杀代替正常关闭，除非专门测试异常生命周期；结束测试时显式关闭串口并确认 `COM6` 可重新打开。
