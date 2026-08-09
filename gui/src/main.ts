import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Activity,
  Bot,
  Cable,
  ChevronDown,
  CircleStop,
  Cpu,
  Eraser,
  Eye,
  EyeOff,
  Gauge,
  MonitorUp,
  Pause,
  Play,
  Plug,
  Radio,
  RefreshCw,
  Send,
  Settings2,
  TerminalSquare,
  Unplug,
  createIcons,
} from "lucide";
import "./styles.css";

type PortInfo = {
  name: string;
  description: string;
  hardware_id?: string;
  available: boolean;
};

type SerialEventData = {
  bytes: number;
  utf8: string;
  hex: string;
};

type SerialEvent = {
  schema_version: number;
  id: string;
  timestamp: string;
  process_id: number;
  source: string;
  event_type: string;
  message: string;
  connection_id?: string;
  port?: string;
  direction?: "tx" | "rx";
  data?: SerialEventData;
  details: Record<string, unknown>;
};

type ConnectionStatus = {
  id: string;
  port: string;
  baud_rate: number;
  connected: boolean;
  bytes_sent: number;
  bytes_received: number;
};

type MirroredConnection = {
  id: string;
  port: string;
  baudRate?: number;
  source: string;
  processId: number;
  connected: boolean;
};

type EventStreamInfo = { address: string; logPath: string };
type ViewFilter = "all" | "rx" | "tx" | "activity";
type DataView = "utf8" | "hex";

const state = {
  ports: [] as PortInfo[],
  events: [] as SerialEvent[],
  eventIds: new Set<string>(),
  mirroredConnections: new Map<string, MirroredConnection>(),
  sharedConnectionIds: new Set<string>(),
  selectedConnectionId: "",
  filter: "all" as ViewFilter,
  dataView: "utf8" as DataView,
  paused: false,
  autoScroll: true,
  streamInfo: null as EventStreamInfo | null,
};

const app = document.querySelector<HTMLDivElement>("#app")!;
app.innerHTML = `
  <div class="app-shell">
    <header class="topbar">
      <div class="brand">
        <div class="brand-mark"><i data-lucide="cable"></i></div>
        <div><strong>Serial MCP</strong><span>Console</span></div>
      </div>
      <div class="bridge-status" id="bridge-status" title="跨进程事件桥接">
        <span class="live-dot"></span>
        <div><strong>事件桥接在线</strong><span id="bridge-address">正在连接…</span></div>
      </div>
      <div class="topbar-meta">
        <div class="metric"><span>进程</span><strong id="process-count">0</strong></div>
        <div class="metric"><span>事件</span><strong id="event-count">0</strong></div>
        <time id="clock">--:--:--</time>
      </div>
    </header>

    <aside class="sidebar">
      <section class="side-section connect-section">
        <div class="section-heading">
          <div><span class="eyebrow">DEVICE</span><h2>连接设备</h2></div>
          <button class="icon-button" id="refresh-ports" title="刷新端口"><i data-lucide="refresh-cw"></i></button>
        </div>
        <label class="field-label" for="port-select">串口</label>
        <div class="select-wrap">
          <select id="port-select"><option value="">正在扫描端口…</option></select>
          <i data-lucide="chevron-down"></i>
        </div>
        <div class="field-grid">
          <label><span>波特率</span><input id="baud-rate" type="number" min="1" max="4000000" value="115200" /></label>
          <label><span>数据位</span><select id="data-bits"><option>8</option><option>7</option><option>6</option><option>5</option></select></label>
          <label><span>停止位</span><select id="stop-bits"><option>1</option><option>2</option></select></label>
          <label><span>校验</span><select id="parity"><option value="none">None</option><option value="even">Even</option><option value="odd">Odd</option></select></label>
        </div>
        <button class="primary-button" id="connect-button"><i data-lucide="plug"></i><span>打开串口</span></button>
      </section>

      <section class="side-section sessions-section">
        <div class="section-heading compact">
          <div><span class="eyebrow">SESSIONS</span><h2>活动连接</h2></div>
          <span class="count-pill" id="connection-count">0</span>
        </div>
        <div class="connection-list" id="connection-list">
          <div class="empty-mini"><i data-lucide="radio"></i><span>暂无连接</span></div>
        </div>
      </section>

      <section class="side-section line-control">
        <div class="section-heading compact"><div><span class="eyebrow">CONTROL</span><h2>控制线</h2></div></div>
        <div class="control-row">
          <button class="control-button" data-line="rts" data-level="true"><span>RTS</span><strong>HIGH</strong></button>
          <button class="control-button" data-line="rts" data-level="false"><span>RTS</span><strong>LOW</strong></button>
        </div>
        <div class="control-row">
          <button class="control-button" data-line="dtr" data-level="true"><span>DTR</span><strong>HIGH</strong></button>
          <button class="control-button" data-line="dtr" data-level="false"><span>DTR</span><strong>LOW</strong></button>
        </div>
      </section>
    </aside>

    <main class="workspace">
      <section class="monitor-panel">
        <div class="panel-toolbar">
          <div class="view-tabs" role="tablist">
            <button class="view-tab active" data-filter="all"><i data-lucide="terminal-square"></i>全部</button>
            <button class="view-tab" data-filter="rx"><span class="direction-dot rx"></span>接收</button>
            <button class="view-tab" data-filter="tx"><span class="direction-dot tx"></span>发送</button>
            <button class="view-tab" data-filter="activity"><i data-lucide="bot"></i>AI 活动</button>
          </div>
          <div class="toolbar-actions">
            <div class="segmented" aria-label="数据显示格式">
              <button class="active" data-data-view="utf8">UTF-8</button><button data-data-view="hex">HEX</button>
            </div>
            <button class="icon-button" id="pause-button" title="暂停显示"><i data-lucide="pause"></i></button>
            <button class="icon-button" id="scroll-button" title="自动滚动"><i data-lucide="eye"></i></button>
            <button class="icon-button" id="clear-button" title="清空当前视图"><i data-lucide="eraser"></i></button>
          </div>
        </div>

        <div class="terminal" id="terminal">
          <div class="terminal-empty" id="terminal-empty">
            <div class="radar"><span></span><i data-lucide="activity"></i></div>
            <strong>等待串口活动</strong>
            <p>MCP、CLI 与 GUI 的操作会实时汇聚到这里</p>
          </div>
          <div class="event-list" id="event-list"></div>
        </div>

        <div class="composer">
          <div class="composer-head">
            <div class="connection-target"><span>发送至</span><strong id="send-target">未选择共享连接</strong></div>
            <div class="composer-options">
              <label>编码 <select id="send-encoding"><option value="utf8">UTF-8</option><option value="hex">HEX</option><option value="base64">Base64</option></select></label>
              <label>行尾 <select id="line-ending"><option value="none">无</option><option value="lf">LF</option><option value="crlf">CRLF</option><option value="cr">CR</option></select></label>
            </div>
          </div>
          <div class="composer-input">
            <textarea id="send-data" rows="2" placeholder="输入要发送的数据 · Ctrl + Enter 发送"></textarea>
            <button id="send-button"><i data-lucide="send"></i><span>发送</span><kbd>⌃↵</kbd></button>
          </div>
        </div>
      </section>

      <aside class="inspector">
        <section class="inspector-section overview">
          <div class="section-heading compact"><div><span class="eyebrow">LIVE OVERVIEW</span><h2>实时概览</h2></div><i data-lucide="monitor-up"></i></div>
          <div class="stats-grid">
            <div class="stat-card rx"><i data-lucide="circle-stop"></i><span>接收</span><strong id="rx-bytes">0 B</strong></div>
            <div class="stat-card tx"><i data-lucide="send"></i><span>发送</span><strong id="tx-bytes">0 B</strong></div>
          </div>
        </section>
        <section class="inspector-section activity-feed-section">
          <div class="section-heading compact"><div><span class="eyebrow">EXTERNAL ACTIVITY</span><h2>进程动态</h2></div><i data-lucide="cpu"></i></div>
          <div class="activity-feed" id="activity-feed">
            <div class="empty-activity">外部 MCP / CLI 操作将显示在此处</div>
          </div>
        </section>
        <section class="inspector-section bridge-card">
          <div class="bridge-card-icon"><i data-lucide="gauge"></i></div>
          <div><span class="eyebrow">EVENT JOURNAL</span><strong>跨进程同步已启用</strong><p id="journal-path">正在读取事件日志位置…</p></div>
        </section>
      </aside>
    </main>
  </div>
  <div class="toast-region" id="toast-region"></div>
`;

createIcons({
  icons: {
    Activity,
    Bot,
    Cable,
    ChevronDown,
    CircleStop,
    Cpu,
    Eraser,
    Eye,
    EyeOff,
    Gauge,
    MonitorUp,
    Pause,
    Play,
    Plug,
    Radio,
    RefreshCw,
    Send,
    Settings2,
    TerminalSquare,
    Unplug,
  },
});

const terminal = byId("terminal");
const eventList = byId("event-list");

function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  return document.getElementById(id) as T;
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function humanBytes(value: number): string {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}

function eventTime(timestamp: string): string {
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    fractionalSecondDigits: 3,
    hour12: false,
  }).format(new Date(timestamp));
}

function sourceLabel(source: string): string {
  const labels: Record<string, string> = { mcp: "MCP", cli: "CLI", gui: "GUI" };
  return labels[source] ?? source.toUpperCase();
}

function addEvent(event: SerialEvent): void {
  if (state.eventIds.has(event.id)) return;
  state.eventIds.add(event.id);
  state.events.push(event);
  if (state.events.length > 2_000) {
    const removed = state.events.splice(0, state.events.length - 2_000);
    removed.forEach((item) => state.eventIds.delete(item.id));
  }

  if (event.connection_id) {
    const existing = state.mirroredConnections.get(event.connection_id);
    if (event.event_type === "connection.opened") {
      state.sharedConnectionIds.add(event.connection_id);
      state.mirroredConnections.set(event.connection_id, {
        id: event.connection_id,
        port: event.port ?? "未知端口",
        baudRate: Number(event.details.baud_rate) || undefined,
        source: event.source,
        processId: event.process_id,
        connected: true,
      });
    } else if (event.event_type === "connection.closed" && existing) {
      existing.connected = false;
      state.sharedConnectionIds.delete(event.connection_id);
      if (state.selectedConnectionId === event.connection_id) state.selectedConnectionId = "";
    } else if (!existing && event.port) {
      state.mirroredConnections.set(event.connection_id, {
        id: event.connection_id,
        port: event.port,
        source: event.source,
        processId: event.process_id,
        connected: event.event_type !== "connection.closed",
      });
    }
  }

  if (!state.paused) renderAll();
}

function filteredEvents(): SerialEvent[] {
  return state.events.filter((event) => {
    if (state.filter === "rx") return event.direction === "rx";
    if (state.filter === "tx") return event.direction === "tx";
    if (state.filter === "activity") return event.source === "mcp" || event.source === "cli";
    return true;
  });
}

function renderAll(): void {
  renderEvents();
  renderConnections();
  renderStats();
  renderActivityFeed();
}

function renderEvents(): void {
  const wasNearBottom = terminal.scrollHeight - terminal.scrollTop - terminal.clientHeight < 90;
  const events = filteredEvents().slice(-600);
  byId("terminal-empty").classList.toggle("hidden", events.length > 0);
  eventList.innerHTML = events
    .map((event) => {
      const isTransfer = event.direction === "rx" || event.direction === "tx";
      const content = event.data
        ? state.dataView === "hex"
          ? event.data.hex || "∅"
          : event.data.utf8 || "∅"
        : event.message;
      const direction = event.direction?.toUpperCase() ?? event.event_type.split(".").at(-1)?.toUpperCase() ?? "EVENT";
      return `
        <article class="event-row ${event.direction ?? "operation"}">
          <time>${eventTime(event.timestamp)}</time>
          <span class="source-badge source-${escapeHtml(event.source)}">${escapeHtml(sourceLabel(event.source))}</span>
          <span class="event-direction">${escapeHtml(direction)}</span>
          <div class="event-content">
            <div class="event-meta">
              <strong>${escapeHtml(event.port ?? "SYSTEM")}</strong>
              ${event.connection_id ? `<span>#${escapeHtml(event.connection_id.slice(0, 8))}</span>` : ""}
              ${isTransfer && event.data ? `<span>${event.data.bytes} bytes</span>` : ""}
              <span>PID ${event.process_id}</span>
            </div>
            <pre>${escapeHtml(content)}</pre>
          </div>
        </article>`;
    })
    .join("");
  if (state.autoScroll && (wasNearBottom || events.length < 20)) terminal.scrollTop = terminal.scrollHeight;
}

function renderConnections(): void {
  const connections = [...state.mirroredConnections.values()].filter((item) => item.connected);
  byId("connection-count").textContent = String(connections.length);
  const list = byId("connection-list");
  if (!connections.length) {
    list.innerHTML = `<div class="empty-mini"><i data-lucide="radio"></i><span>暂无连接</span></div>`;
  } else {
    list.innerHTML = connections
      .map((connection) => {
        const shared = state.sharedConnectionIds.has(connection.id);
        const selected = state.selectedConnectionId === connection.id;
        return `
          <div class="connection-item ${selected ? "selected" : ""}">
            <button class="connection-select" data-connection-id="${escapeHtml(connection.id)}">
              <span class="connection-icon ${shared ? "local" : "mirror"}"><i data-lucide="${connection.source === "mcp" ? "bot" : "plug"}"></i></span>
              <span class="connection-copy"><strong>${escapeHtml(connection.port)}</strong><small>${connection.baudRate ? `${connection.baudRate.toLocaleString()} baud · ` : ""}共享实例 · ${sourceLabel(connection.source)} 创建</small></span>
              <span class="connection-live"></span>
            </button>
            <button class="disconnect-button" data-close-id="${escapeHtml(connection.id)}" title="关闭共享连接"><i data-lucide="unplug"></i></button>
          </div>`;
      })
      .join("");
  }
  const selected = state.mirroredConnections.get(state.selectedConnectionId);
  byId("send-target").textContent = selected && state.sharedConnectionIds.has(selected.id) ? selected.port : "未选择共享连接";
  createIcons({ icons: { Bot, Plug, Radio, Unplug } });
}

function renderStats(): void {
  let rx = 0;
  let tx = 0;
  const processes = new Set<number>();
  state.events.forEach((event) => {
    processes.add(event.process_id);
    if (event.direction === "rx") rx += event.data?.bytes ?? 0;
    if (event.direction === "tx") tx += event.data?.bytes ?? 0;
  });
  byId("rx-bytes").textContent = humanBytes(rx);
  byId("tx-bytes").textContent = humanBytes(tx);
  byId("event-count").textContent = state.events.length.toLocaleString();
  byId("process-count").textContent = String(processes.size);
}

function renderActivityFeed(): void {
  const activity = state.events
    .filter((event) => event.source === "mcp" || event.source === "cli")
    .slice(-8)
    .reverse();
  byId("activity-feed").innerHTML = activity.length
    ? activity
        .map(
          (event) => `
          <div class="activity-item">
            <span class="activity-icon source-${escapeHtml(event.source)}"><i data-lucide="${event.source === "mcp" ? "bot" : "terminal-square"}"></i></span>
            <div><strong>${escapeHtml(event.message)}</strong><span>${escapeHtml(sourceLabel(event.source))} · PID ${event.process_id} · ${eventTime(event.timestamp)}</span></div>
          </div>`,
        )
        .join("")
    : `<div class="empty-activity">外部 MCP / CLI 操作将显示在此处</div>`;
  createIcons({ icons: { Bot, TerminalSquare } });
}

async function refreshPorts(): Promise<void> {
  const refresh = byId<HTMLButtonElement>("refresh-ports");
  refresh.classList.add("spinning");
  try {
    state.ports = await invoke<PortInfo[]>("list_ports");
    const select = byId<HTMLSelectElement>("port-select");
    const current = select.value;
    select.innerHTML = state.ports.length
      ? state.ports
          .map((port) => `<option value="${escapeHtml(port.name)}">${escapeHtml(port.name)} · ${escapeHtml(port.description)}</option>`)
          .join("")
      : `<option value="">未发现串口</option>`;
    if (state.ports.some((port) => port.name === current)) select.value = current;
  } catch (error) {
    showToast("扫描串口失败", String(error), "error");
  } finally {
    window.setTimeout(() => refresh.classList.remove("spinning"), 350);
  }
}

async function openPort(): Promise<void> {
  const port = byId<HTMLSelectElement>("port-select").value;
  if (!port) return showToast("没有可用串口", "请连接设备后重新扫描。", "error");
  const button = byId<HTMLButtonElement>("connect-button");
  button.disabled = true;
  try {
    const response = await invoke<{ connectionId: string }>("open_port", {
      request: {
        port,
        baudRate: Number(byId<HTMLInputElement>("baud-rate").value),
        dataBits: byId<HTMLSelectElement>("data-bits").value,
        stopBits: byId<HTMLSelectElement>("stop-bits").value,
        parity: byId<HTMLSelectElement>("parity").value,
        flowControl: "none",
      },
    });
    state.sharedConnectionIds.add(response.connectionId);
    state.selectedConnectionId = response.connectionId;
    await syncSharedConnections();
    showToast("串口已打开", `${port} 已加入实时监听。`, "success");
  } catch (error) {
    showToast("打开串口失败", String(error), "error");
  } finally {
    button.disabled = false;
  }
}

async function syncSharedConnections(): Promise<void> {
  const connections = await invoke<ConnectionStatus[]>("list_connections");
  const activeIds = new Set(connections.map((connection) => connection.id));
  state.mirroredConnections.forEach((connection, id) => {
    if (!activeIds.has(id)) connection.connected = false;
  });
  state.sharedConnectionIds.clear();
  for (const connection of connections) {
    state.sharedConnectionIds.add(connection.id);
    const current = state.mirroredConnections.get(connection.id);
    state.mirroredConnections.set(connection.id, {
      id: connection.id,
      port: connection.port,
      baudRate: connection.baud_rate,
      source: current?.source ?? "gui",
      processId: current?.processId ?? 0,
      connected: connection.connected,
    });
  }
  renderConnections();
}

async function sendData(): Promise<void> {
  const connectionId = state.selectedConnectionId;
  if (!connectionId || !state.sharedConnectionIds.has(connectionId)) {
    return showToast("请选择共享连接", "请先选择由 GUI、MCP 或 CLI 打开的连接。", "error");
  }
  const textarea = byId<HTMLTextAreaElement>("send-data");
  const encoding = byId<HTMLSelectElement>("send-encoding").value;
  let data = textarea.value;
  if (!data) return;
  if (encoding === "utf8") {
    const endings: Record<string, string> = { none: "", lf: "\n", crlf: "\r\n", cr: "\r" };
    data += endings[byId<HTMLSelectElement>("line-ending").value];
  }
  const button = byId<HTMLButtonElement>("send-button");
  button.disabled = true;
  try {
    await invoke("write_serial", { connectionId, data, encoding });
    textarea.value = "";
    textarea.focus();
  } catch (error) {
    showToast("发送失败", String(error), "error");
  } finally {
    button.disabled = false;
  }
}

function showToast(title: string, message: string, type: "success" | "error"): void {
  const toast = document.createElement("div");
  toast.className = `toast ${type}`;
  toast.innerHTML = `<strong>${escapeHtml(title)}</strong><span>${escapeHtml(message)}</span>`;
  byId("toast-region").append(toast);
  window.setTimeout(() => toast.classList.add("show"), 10);
  window.setTimeout(() => {
    toast.classList.remove("show");
    window.setTimeout(() => toast.remove(), 250);
  }, 4_200);
}

function bindUi(): void {
  byId("refresh-ports").addEventListener("click", refreshPorts);
  byId("connect-button").addEventListener("click", openPort);
  byId("send-button").addEventListener("click", sendData);
  byId("send-data").addEventListener("keydown", (event) => {
    if (event.ctrlKey && event.key === "Enter") {
      event.preventDefault();
      void sendData();
    }
  });

  document.querySelectorAll<HTMLButtonElement>("[data-filter]").forEach((button) => {
    button.addEventListener("click", () => {
      state.filter = button.dataset.filter as ViewFilter;
      document.querySelectorAll("[data-filter]").forEach((item) => item.classList.remove("active"));
      button.classList.add("active");
      renderEvents();
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-data-view]").forEach((button) => {
    button.addEventListener("click", () => {
      state.dataView = button.dataset.dataView as DataView;
      document.querySelectorAll("[data-data-view]").forEach((item) => item.classList.remove("active"));
      button.classList.add("active");
      renderEvents();
    });
  });

  byId("pause-button").addEventListener("click", () => {
    state.paused = !state.paused;
    byId("pause-button").classList.toggle("active", state.paused);
    byId("pause-button").innerHTML = `<i data-lucide="${state.paused ? "play" : "pause"}"></i>`;
    createIcons({ icons: { Pause, Play } });
    if (!state.paused) renderAll();
  });
  byId("scroll-button").addEventListener("click", () => {
    state.autoScroll = !state.autoScroll;
    byId("scroll-button").classList.toggle("active", !state.autoScroll);
    byId("scroll-button").innerHTML = `<i data-lucide="${state.autoScroll ? "eye" : "eye-off"}"></i>`;
    createIcons({ icons: { Eye, EyeOff } });
  });
  byId("clear-button").addEventListener("click", () => {
    state.events = [];
    state.eventIds.clear();
    renderAll();
  });

  byId("connection-list").addEventListener("click", async (event) => {
    const closeTarget = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-close-id]");
    if (closeTarget) {
      const id = closeTarget.dataset.closeId!;
      try {
        await invoke("close_port", { connectionId: id });
        state.sharedConnectionIds.delete(id);
        if (state.selectedConnectionId === id) state.selectedConnectionId = "";
        const mirrored = state.mirroredConnections.get(id);
        if (mirrored) mirrored.connected = false;
        renderAll();
      } catch (error) {
        showToast("关闭串口失败", String(error), "error");
      }
      return;
    }
    const target = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-connection-id]");
    if (!target) return;
    const id = target.dataset.connectionId!;
    state.selectedConnectionId = id;
    renderConnections();
  });

  document.querySelectorAll<HTMLButtonElement>("[data-line]").forEach((button) => {
    button.addEventListener("click", async () => {
      const connectionId = state.selectedConnectionId;
      if (!connectionId || !state.sharedConnectionIds.has(connectionId)) {
        return showToast("请选择共享连接", "GUI、MCP 与 CLI 均可操作同一个共享连接。", "error");
      }
      const line = button.dataset.line!;
      const level = button.dataset.level === "true";
      try {
        await invoke("set_control_lines", {
          connectionId,
          rts: line === "rts" ? level : null,
          dtr: line === "dtr" ? level : null,
        });
      } catch (error) {
        showToast("控制线设置失败", String(error), "error");
      }
    });
  });
}

async function initialize(): Promise<void> {
  bindUi();
  if (!("__TAURI_INTERNALS__" in window)) {
    byId("bridge-status").classList.add("offline");
    byId("bridge-address").textContent = "浏览器预览 · IPC 未连接";
    byId("journal-path").textContent = "请在 Tauri 窗口中运行以连接共享代理";
    byId<HTMLSelectElement>("port-select").innerHTML = `<option value="">Tauri 中扫描串口</option>`;
    byId<HTMLButtonElement>("connect-button").disabled = true;
    renderAll();
    window.setInterval(() => {
      byId("clock").textContent = new Date().toLocaleTimeString("zh-CN", { hour12: false });
    }, 1_000);
    return;
  }
  await listen<SerialEvent>("serial-event", ({ payload }) => addEvent(payload));

  try {
    const [history, streamInfo] = await Promise.all([
      invoke<SerialEvent[]>("event_history", { limit: 1_000 }),
      invoke<EventStreamInfo>("event_stream_info"),
    ]);
    history.forEach(addEvent);
    state.events.sort((left, right) => left.timestamp.localeCompare(right.timestamp));
    state.streamInfo = streamInfo;
    byId("bridge-address").textContent = streamInfo.address;
    byId("journal-path").textContent = streamInfo.logPath;
    byId("journal-path").title = streamInfo.logPath;
  } catch (error) {
    byId("bridge-status").classList.add("offline");
    byId("bridge-address").textContent = "桥接不可用";
    showToast("事件桥接初始化失败", String(error), "error");
  }

  await refreshPorts();
  await syncSharedConnections();
  renderAll();
  window.setInterval(() => {
    byId("clock").textContent = new Date().toLocaleTimeString("zh-CN", { hour12: false });
  }, 1_000);
}

void initialize();
