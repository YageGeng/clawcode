const state = {
  socket: null,
  nextId: 1,
  pending: new Map(),
  currentSessionId: null,
  sessionCursor: null,
  running: false,
  currentAssistant: null,
  currentThought: null,
  pendingPermissions: new Map(),
  toolBlocks: new Map(),
  modelConfig: null,
  latestProviderUsage: null,
};

const els = {
  connectionStatus: document.getElementById("connectionStatus"),
  sessions: document.getElementById("sessions"),
  loadMoreSessionsButton: document.getElementById("loadMoreSessionsButton"),
  newSessionButton: document.getElementById("newSessionButton"),
  sessionLabel: document.getElementById("sessionLabel"),
  runState: document.getElementById("runState"),
  sendCtxButton: document.getElementById("sendCtxButton"),
  ctxMeter: document.getElementById("ctxMeter"),
  ctxMeterValue: document.getElementById("ctxMeterValue"),
  ctxTooltipContext: document.getElementById("ctxTooltipContext"),
  ctxTooltipTokens: document.getElementById("ctxTooltipTokens"),
  modelSelect: document.getElementById("modelSelect"),
  modelSelectButton: document.getElementById("modelSelectButton"),
  modelSelectLabel: document.getElementById("modelSelectLabel"),
  modelOptionsMenu: document.getElementById("modelOptionsMenu"),
  transcript: document.getElementById("transcript"),
  composer: document.getElementById("composer"),
  promptInput: document.getElementById("promptInput"),
  sendButton: document.getElementById("sendButton"),
  cancelButton: document.getElementById("cancelButton"),
  permissionModal: document.getElementById("permissionModal"),
  permissionTitle: document.getElementById("permissionTitle"),
  permissionBody: document.getElementById("permissionBody"),
  permissionOptions: document.getElementById("permissionOptions"),
};

const MARKDOWN_OPTIONS = {
  gfm: true,
  breaks: true,
};

// Opens the ACP WebSocket and starts the initial session workflow.
function connect() {
  setStatus("Connecting");
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  state.socket = new WebSocket(`${scheme}://${window.location.host}/acp`);
  state.socket.addEventListener("open", onSocketOpen);
  state.socket.addEventListener("message", onSocketMessage);
  state.socket.addEventListener("close", onSocketClose);
  state.socket.addEventListener("error", onSocketError);
}

// Initializes the ACP connection after the WebSocket opens.
async function onSocketOpen() {
  try {
    setStatus("Connected");
    await sendRequest("initialize", {
      protocolVersion: 1,
      clientCapabilities: {},
    });
    await refreshSessions();
    await createSession();
  } catch (error) {
    addSystemMessage(`Initialization failed: ${error.message}`);
  }
}

// Clears in-flight UI state when the ACP WebSocket closes.
function onSocketClose() {
  setStatus("Disconnected");
  rejectPendingRequests(new Error("WebSocket disconnected"));
  clearPermissionRequests();
  setRunning(false);
}

// Clears in-flight UI state when the ACP WebSocket reports a connection error.
function onSocketError() {
  setStatus("Connection error");
  rejectPendingRequests(new Error("WebSocket connection error"));
  clearPermissionRequests();
  setRunning(false);
}

// Parses a WebSocket frame and dispatches JSON-RPC messages.
function onSocketMessage(event) {
  let message;
  try {
    message = JSON.parse(event.data);
  } catch (error) {
    addSystemMessage(`Malformed ACP message: ${error.message}`);
    return;
  }
  handleMessage(message);
}

// Routes a JSON-RPC response, request, or notification.
function handleMessage(message) {
  if (Object.prototype.hasOwnProperty.call(message, "id") && !message.method) {
    settleResponse(message);
    return;
  }

  if (message.method === "session/update") {
    applySessionUpdate(message.params);
    return;
  }

  if (message.method === "session/request_permission") {
    showPermissionRequest(message.id, message.params);
    return;
  }

  if (message.id) {
    respondError(
      message.id,
      -32601,
      `Unsupported client request: ${message.method}`,
    );
  }
}

// Resolves or rejects a pending request promise.
function settleResponse(message) {
  const pending = state.pending.get(message.id);
  if (!pending) {
    return;
  }
  state.pending.delete(message.id);
  if (message.error) {
    pending.reject(new Error(message.error.message || "ACP request failed"));
  } else {
    pending.resolve(message.result);
  }
}

// Sends a JSON-RPC request and returns a promise for its response.
function sendRequest(method, params = {}) {
  if (!isSocketOpen()) {
    return Promise.reject(new Error("WebSocket is not connected"));
  }
  const id = state.nextId++;
  const payload = { jsonrpc: "2.0", id, method, params };
  return new Promise((resolve, reject) => {
    state.pending.set(id, { resolve, reject });
    try {
      state.socket.send(JSON.stringify(payload));
    } catch (error) {
      state.pending.delete(id);
      reject(error);
    }
  });
}

// Sends a JSON-RPC notification without waiting for a response.
function sendNotification(method, params = {}) {
  if (!isSocketOpen()) {
    return;
  }
  state.socket.send(JSON.stringify({ jsonrpc: "2.0", method, params }));
}

// Sends a JSON-RPC result for an agent-to-client request.
function respondResult(id, result) {
  if (!isSocketOpen()) {
    return;
  }
  state.socket.send(JSON.stringify({ jsonrpc: "2.0", id, result }));
}

// Sends a JSON-RPC error for an unsupported agent-to-client request.
function respondError(id, code, message) {
  if (!isSocketOpen()) {
    return;
  }
  state.socket.send(
    JSON.stringify({ jsonrpc: "2.0", id, error: { code, message } }),
  );
}

// Returns whether the ACP WebSocket can currently send messages.
function isSocketOpen() {
  return state.socket && state.socket.readyState === WebSocket.OPEN;
}

// Rejects all outstanding client-to-agent requests after transport failure.
function rejectPendingRequests(error) {
  for (const pending of state.pending.values()) {
    pending.reject(error);
  }
  state.pending.clear();
}

// Updates the visible connection status.
function setStatus(text) {
  els.connectionStatus.textContent = text;
}

// Updates the visible running state and button availability.
function setRunning(running) {
  state.running = running;
  els.runState.textContent = running ? "Running" : "Idle";
  els.sendButton.disabled = running;
  els.cancelButton.classList.toggle("hidden", !running);
}

// Creates a new ACP session in the server process working directory.
async function createSession() {
  if (state.running) {
    return;
  }
  clearTranscript();
  state.currentSessionId = null;
  updateSessionLabel();
  try {
    const response = await sendRequest("session/new", {
      cwd: ".",
      mcpServers: [],
    });
    state.currentSessionId = response.sessionId;
    updateSessionLabel();
    applyConfigOptions(response.configOptions || []);
    addSystemMessage(`Session created: ${state.currentSessionId}`);
  } catch (error) {
    addSystemMessage(`Session creation failed: ${error.message}`);
  }
}

// Loads an existing ACP session and replays its history through session updates.
async function loadSession(sessionId) {
  if (state.running) {
    return;
  }
  const previousSessionId = state.currentSessionId;
  clearTranscript();
  state.currentSessionId = sessionId;
  updateSessionLabel();
  try {
    const response = await sendRequest("session/load", {
      sessionId,
      cwd: ".",
      mcpServers: [],
    });
    applyConfigOptions(response.configOptions || []);
    addSystemMessage(`Session loaded: ${state.currentSessionId}`);
  } catch (error) {
    state.currentSessionId = previousSessionId;
    updateSessionLabel();
    addSystemMessage(`Session load failed: ${error.message}`);
  }
}

// Refreshes the persisted session list.
async function refreshSessions(cursor = null) {
  const params = cursor ? { cursor } : {};
  const response = await sendRequest("session/list", params);
  renderSessions(response.sessions || [], Boolean(cursor));
  state.sessionCursor = response.nextCursor || null;
  els.loadMoreSessionsButton.classList.toggle("hidden", !state.sessionCursor);
}

// Renders persisted sessions into the sidebar.
function renderSessions(sessions, append) {
  if (!append) {
    els.sessions.replaceChildren();
  }
  for (const session of sessions) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "session-button";
    button.addEventListener("click", () => loadSession(session.sessionId));
    const title = document.createElement("div");
    title.className = "session-title";
    title.textContent = session.title || session.sessionId;
    const meta = document.createElement("div");
    meta.className = "session-meta";
    meta.textContent = session.cwd || "";
    button.append(title, meta);
    els.sessions.append(button);
  }
}

// Updates session label text from current state.
function updateSessionLabel() {
  els.sessionLabel.textContent = state.currentSessionId
    ? `Session ${state.currentSessionId}`
    : "No session";
}

// Applies ACP config options and updates the model selector.
function applyConfigOptions(configOptions) {
  state.modelConfig = findModelConfig(configOptions);
  els.modelSelect.replaceChildren();
  els.modelOptionsMenu.replaceChildren();
  if (!state.modelConfig) {
    els.modelSelect.disabled = true;
    els.modelSelectButton.disabled = true;
    els.modelSelectLabel.textContent = "Model";
    closeModelMenu();
    return;
  }

  const options = flattenSelectOptions(state.modelConfig.options);
  for (const option of options) {
    const item = document.createElement("option");
    item.value = option.value;
    item.textContent = option.name || option.value;
    item.selected = option.value === state.modelConfig.currentValue;
    els.modelSelect.append(item);
  }
  els.modelSelect.disabled = false;
  renderModelOptions(options);
}

// Renders the custom model dropdown list so the visible popup can be styled consistently.
function renderModelOptions(options) {
  els.modelOptionsMenu.replaceChildren();
  const selected = selectedModelOption(options);
  els.modelSelectLabel.textContent =
    selected?.name || selected?.value || "Model";

  for (const option of options) {
    const item = document.createElement("button");
    item.type = "button";
    item.className = "model-option";
    item.dataset.value = option.value;
    item.setAttribute("role", "option");
    item.setAttribute(
      "aria-selected",
      String(option.value === state.modelConfig.currentValue),
    );

    const name = document.createElement("span");
    name.className = "model-option-name";
    name.textContent = option.name || option.value;

    const value = document.createElement("span");
    value.className = "model-option-value";
    value.textContent = option.value;

    item.append(name, value);
    item.addEventListener("click", () => chooseModelOption(option.value));
    els.modelOptionsMenu.append(item);
  }

  els.modelSelectButton.disabled = options.length === 0;
  closeModelMenu();
}

// Returns the currently selected model option from a flattened ACP option list.
function selectedModelOption(options) {
  return options.find(
    (option) => option.value === state.modelConfig?.currentValue,
  );
}

// Finds the ACP model config option in a session response.
function findModelConfig(configOptions) {
  const config = configOptions.find(
    (option) => option.id === "model" || option.category === "model",
  );
  if (!config || config.type !== "select") {
    return null;
  }
  return {
    id: config.id,
    currentValue: config.currentValue,
    options: config.options || [],
  };
}

// Flattens grouped and ungrouped ACP select options.
function flattenSelectOptions(options) {
  if (!Array.isArray(options)) {
    return [];
  }
  if (options.length > 0 && Array.isArray(options[0].options)) {
    return options.flatMap((group) => group.options || []);
  }
  return options;
}

// Sends a model config update for the active session.
async function switchModel(value) {
  if (!state.currentSessionId || !state.modelConfig || !value) {
    return;
  }
  try {
    const response = await sendRequest("session/set_config_option", {
      sessionId: state.currentSessionId,
      configId: state.modelConfig.id,
      value,
    });
    applyConfigOptions(response.configOptions || []);
    addSystemMessage(`Model switched: ${value}`);
  } catch (error) {
    addSystemMessage(`Model switch failed: ${error.message}`);
  }
}

// Opens or closes the custom model dropdown.
function toggleModelMenu() {
  if (els.modelSelectButton.disabled) {
    return;
  }
  const expanded =
    els.modelSelectButton.getAttribute("aria-expanded") === "true";
  if (expanded) {
    closeModelMenu();
    return;
  }
  els.modelOptionsMenu.classList.remove("hidden");
  els.modelSelectButton.setAttribute("aria-expanded", "true");
}

// Closes the custom model dropdown.
function closeModelMenu() {
  els.modelOptionsMenu.classList.add("hidden");
  els.modelSelectButton.setAttribute("aria-expanded", "false");
}

// Selects a model from the custom dropdown and delegates the ACP config update.
function chooseModelOption(value) {
  closeModelMenu();
  els.modelSelect.value = value;
  switchModel(value);
}

// Sends the current prompt text as an ACP prompt request.
async function submitPrompt() {
  const text = els.promptInput.value.trim();
  if (!text || !state.currentSessionId || state.running) {
    return;
  }

  els.promptInput.value = "";
  state.currentAssistant = null;
  state.currentThought = null;
  addMessage("user", text);
  setRunning(true);
  try {
    const response = await sendRequest("session/prompt", {
      sessionId: state.currentSessionId,
      prompt: [{ type: "text", text }],
    });
    addSystemMessage(`Turn finished: ${response.stopReason || "end_turn"}`);
  } catch (error) {
    addSystemMessage(`Prompt failed: ${error.message}`);
  } finally {
    setRunning(false);
  }
}

// Cancels the currently running prompt turn.
function cancelPrompt() {
  if (!state.currentSessionId || !state.running) {
    return;
  }
  cancelPendingPermissions();
  sendNotification("session/cancel", { sessionId: state.currentSessionId });
  addSystemMessage("Cancel requested.");
}

// Applies an ACP session/update notification to the transcript.
function applySessionUpdate(params) {
  if (!params || !params.update) {
    return;
  }
  if (params.sessionId !== state.currentSessionId) {
    return;
  }
  const update = params.update;
  switch (update.sessionUpdate) {
    case "user_message_chunk":
      appendUserText(contentText(update.content));
      break;
    case "agent_message_chunk":
      appendAssistantText(contentText(update.content));
      break;
    case "agent_thought_chunk":
      appendThoughtText(contentText(update.content));
      break;
    case "tool_call":
      if (isMetadataOnlyToolUpdate(update)) {
        return;
      }
      renderToolUpdate(update);
      break;
    case "tool_call_update":
      if (isMetadataOnlyToolUpdate(update)) {
        return;
      }
      renderToolUpdate(update);
      break;
    case "available_commands_update":
      break;
    case "usage_update":
      renderUsageUpdate(update);
      break;
    default:
      addMessage(
        "system",
        `Update: ${update.sessionUpdate || JSON.stringify(update)}`,
      );
  }
}

// Extracts display text from an ACP content chunk.
function contentText(content) {
  if (!content) {
    return "";
  }
  if (content.type === "text") {
    return content.text || "";
  }
  return JSON.stringify(content);
}

// Updates the context ring and hover details from an ACP usage update.
function renderUsageUpdate(update) {
  updateProviderUsage(update);
  const context = formatContextUsage(update);
  const tokens = formatTokenUsage(state.latestProviderUsage);
  els.ctxMeter.style.setProperty("--ctx-percent", `${context.percent || 0}%`);
  els.ctxMeter.style.setProperty("--ctx-color", context.color);
  els.ctxMeterValue.textContent = context.label;
  els.ctxTooltipContext.textContent = context.tooltip;
  els.ctxTooltipTokens.textContent = tokens.tooltip;
  els.ctxMeter.title = `${context.tooltip}\n${tokens.tooltip}`;
  els.sendCtxButton.title = `${context.tooltip}\n${tokens.tooltip}`;
  els.sendCtxButton.setAttribute(
    "aria-label",
    `${context.tooltip}. ${tokens.tooltip}`,
  );
  els.ctxMeter.setAttribute(
    "aria-label",
    `${context.tooltip}. ${tokens.tooltip}`,
  );
}

// Stores the latest provider usage because ACP can send ctx-only updates afterward.
function updateProviderUsage(update) {
  const usage = providerUsage(update);
  if (usage) {
    state.latestProviderUsage = usage;
  }
}

// Resets the context meter when a new transcript is loaded.
function resetUsageMeter() {
  state.latestProviderUsage = null;
  els.ctxMeter.style.setProperty("--ctx-percent", "0%");
  els.ctxMeter.style.setProperty("--ctx-color", "#8a94a6");
  els.ctxMeterValue.textContent = "0";
  els.ctxTooltipContext.textContent = "ctx: 0";
  els.ctxTooltipTokens.textContent = "tokens: -";
  els.ctxMeter.title = "ctx: 0\ntokens: -";
  els.sendCtxButton.title = "ctx: 0\ntokens: -";
  els.sendCtxButton.setAttribute(
    "aria-label",
    "Context usage unavailable",
  );
  els.ctxMeter.setAttribute("aria-label", "Context usage unavailable");
}

// Formats ACP context-window usage for both the ring and tooltip.
function formatContextUsage(update) {
  const used = numericField(update, "used");
  const size = numericField(update, "size");
  if (size <= 0) {
    return {
      label: compactNumber(used),
      percent: 0,
      color: "#8a94a6",
      tooltip: `ctx: ${formatNumber(used)}`,
    };
  }

  const percent = Math.min(100, Math.floor((used * 100) / size));
  return {
    label: `${percent}%`,
    percent,
    color: contextUsageColor(percent),
    tooltip: `ctx: ${formatNumber(used)} / ${formatNumber(size)} (${percent}%)`,
  };
}

// Formats provider-reported token usage metadata for the hover tooltip.
function formatTokenUsage(usage) {
  if (!usage) {
    return { tooltip: "tokens: -" };
  }

  const input = numericField(usage, "input_tokens", "inputTokens");
  const output = numericField(usage, "output_tokens", "outputTokens");
  const total =
    numericField(usage, "total_tokens", "totalTokens") || input + output;
  const cached = numericField(
    usage,
    "cached_input_tokens",
    "cachedInputTokens",
  );
  const cacheCreated = numericField(
    usage,
    "cache_creation_input_tokens",
    "cacheCreationInputTokens",
  );
  const cacheRate = cacheHitRatePercent(input, cached);
  const cacheText =
    cacheRate === null
      ? `cache ${formatNumber(cached)} read / ${formatNumber(cacheCreated)} write`
      : `cache ${formatNumber(cached)} read / ${formatNumber(cacheCreated)} write (${cacheRate}%)`;

  return {
    tooltip: `tokens: total ${formatNumber(total)} · input ${formatNumber(input)} · output ${formatNumber(output)} · ${cacheText}`,
  };
}

// Extracts Clawcode provider usage from ACP metadata in either public or wire-key form.
function providerUsage(update) {
  const meta = update.meta || update._meta;
  return meta?.clawcode?.usage || null;
}

// Returns the provider cache hit rate as an integer percentage when input tokens are available.
function cacheHitRatePercent(inputTokens, cachedInputTokens) {
  if (inputTokens <= 0) {
    return null;
  }
  return Math.floor((cachedInputTokens * 100) / inputTokens);
}

// Reads a numeric field from snake_case or camelCase ACP payload shapes.
function numericField(value, ...names) {
  for (const name of names) {
    const number = Number(value?.[name]);
    if (Number.isFinite(number)) {
      return number;
    }
  }
  return 0;
}

// Returns the ring color used for the current context-window occupancy.
function contextUsageColor(percent) {
  if (percent >= 90) {
    return "#d92d20";
  }
  if (percent >= 70) {
    return "#b7791f";
  }
  return "#1f9d55";
}

// Formats large token counts with locale-aware separators.
function formatNumber(value) {
  return new Intl.NumberFormat().format(value);
}

// Formats the center label when only a raw context token count is available.
function compactNumber(value) {
  return new Intl.NumberFormat(undefined, {
    notation: "compact",
    maximumFractionDigits: 1,
  }).format(value);
}

// Adds a committed user message and ends any active streamed text block.
function appendUserText(text) {
  if (!text) {
    return;
  }
  resetActiveTextStreams();
  addMessage("user", text);
}

// Appends streamed assistant text into a single active assistant bubble.
function appendAssistantText(text) {
  if (!text) {
    return;
  }
  state.currentThought = null;
  if (!state.currentAssistant) {
    state.currentAssistant = addMessage("assistant", "");
  }
  appendMessageMarkdown(state.currentAssistant, text);
  scrollTranscript();
}

// Appends streamed reasoning text into a single active thought bubble.
function appendThoughtText(text) {
  if (!text) {
    return;
  }
  state.currentAssistant = null;
  if (!state.currentThought) {
    state.currentThought = addMessage("thought", "");
  }
  appendMessageMarkdown(state.currentThought, text);
  scrollTranscript();
}

// Adds one transcript message and returns its element.
function addMessage(kind, text) {
  const message = document.createElement("div");
  message.className = `message ${kind}`;
  els.transcript.append(message);
  setMessageMarkdown(message, text);
  scrollTranscript();
  return message;
}

// Replaces a message body with sanitized Markdown while preserving the raw text for streaming.
function setMessageMarkdown(message, text) {
  message.dataset.rawText = text || "";
  message.classList.add("markdown-body");
  renderMarkdown(message, message.dataset.rawText);
}

// Appends streamed Markdown text to an existing message without losing prior chunks.
function appendMessageMarkdown(message, text) {
  setMessageMarkdown(message, `${message.dataset.rawText || ""}${text}`);
}

// Renders Markdown with a safe plain-text fallback when CDN libraries are unavailable.
function renderMarkdown(target, markdown) {
  if (!window.marked || !window.DOMPurify) {
    target.textContent = markdown;
    return;
  }

  const html = window.marked.parse(markdown, MARKDOWN_OPTIONS);
  target.innerHTML = window.DOMPurify.sanitize(html);
  highlightCodeBlocks(target);
}

// Highlights code blocks when highlight.js has loaded.
function highlightCodeBlocks(root) {
  if (!window.hljs) {
    return;
  }

  for (const block of root.querySelectorAll("pre code")) {
    window.hljs.highlightElement(block);
  }
}

// Ends active streamed assistant and reasoning blocks before another entry type is inserted.
function resetActiveTextStreams() {
  state.currentAssistant = null;
  state.currentThought = null;
}

// Renders a tool-call update into one stable block keyed by tool call ID.
function renderToolUpdate(update) {
  resetActiveTextStreams();
  const block = ensureToolBlock(update);
  updateToolBlockHeader(block, update);

  const params = toolParams(update);
  if (params !== undefined) {
    appendCollapsedToolSection(block, "params", "Parameters", params, false);
  }

  const result = toolResult(update);
  if (result !== undefined) {
    appendCollapsedToolSection(block, "result", "Result", result, true);
  }

  scrollTranscript();
}

// Returns whether a tool update only carries metadata for non-chat UI surfaces.
function isMetadataOnlyToolUpdate(update) {
  return (
    update.toolCallId === "clawcode-subagents" &&
    update.content === undefined &&
    update.rawInput === undefined &&
    update.rawOutput === undefined &&
    update.status === undefined
  );
}

// Returns the existing tool block for an update or creates a new one.
function ensureToolBlock(update) {
  const key = toolBlockKey(update);
  const existing = state.toolBlocks.get(key);
  if (existing) {
    return existing;
  }

  const message = document.createElement("div");
  message.className = "message tool tool-block";

  const header = document.createElement("div");
  header.className = "tool-header";

  const title = document.createElement("div");
  title.className = "tool-title";

  const status = document.createElement("div");
  status.className = "tool-status hidden";

  const statusDot = document.createElement("span");
  statusDot.className = "tool-status-dot";

  const statusText = document.createElement("span");
  statusText.className = "tool-status-text";

  const body = document.createElement("div");
  body.className = "tool-body";

  status.append(statusDot, statusText);
  header.append(title, status);
  message.append(header, body);
  els.transcript.append(message);

  const block = {
    title,
    status,
    statusDot,
    statusText,
    body,
    sections: new Map(),
  };
  state.toolBlocks.set(key, block);
  return block;
}

// Updates the visible title and status for a tool block.
function updateToolBlockHeader(block, update) {
  const title =
    update.title ||
    update.name ||
    block.title.textContent ||
    update.toolCallId ||
    "Tool call";
  block.title.textContent = title;

  if (update.status) {
    const statusKind = toolStatusKind(update.status);
    block.statusText.textContent = update.status.replaceAll("_", " ");
    block.status.classList.remove(
      "tool-status-neutral",
      "tool-status-success",
      "tool-status-error",
    );
    block.status.classList.add(`tool-status-${statusKind}`);
    block.status.classList.remove("hidden");
  }
}

// Maps ACP tool-call statuses to visual status categories.
function toolStatusKind(status) {
  if (status === "completed") {
    return "success";
  }
  if (["failed", "error", "rejected", "cancelled"].includes(status)) {
    return "error";
  }
  return "neutral";
}

// Appends or replaces one collapsed section inside a tool block.
function appendCollapsedToolSection(block, key, title, value, append) {
  const text = formatToolValue(value);
  let section = block.sections.get(key);
  if (!section) {
    const details = document.createElement("details");
    details.className = `tool-section tool-section-${key}`;

    const summary = document.createElement("summary");
    summary.textContent = title;

    const pre = document.createElement("pre");
    pre.className = "tool-code";
    const code = document.createElement("code");

    pre.append(code);
    details.append(summary, pre);
    block.body.append(details);

    section = { code, text: "" };
    block.sections.set(key, section);
  }

  section.text = append ? `${section.text}${text}` : text;
  renderToolJson(section.code, section.text);
}

// Renders tool payload text as JSON when possible and syntax-highlights the code block.
function renderToolJson(code, value) {
  const formatted = formatToolJson(value);
  code.className =
    formatted.language === "json" ? "language-json" : "language-plaintext";
  code.textContent = formatted.text;
  if (window.hljs) {
    delete code.dataset.highlighted;
    window.hljs.highlightElement(code);
  }
}

// Formats JSON-like tool payloads as pretty JSON with a plaintext fallback.
function formatToolJson(value) {
  const parsed = parseJsonValue(value);
  if (parsed.ok) {
    return {
      language: "json",
      text: JSON.stringify(parsed.value, null, 2),
    };
  }
  return {
    language: "plaintext",
    text: String(value ?? ""),
  };
}

// Parses structured JSON values without treating arbitrary tool output as HTML.
function parseJsonValue(value) {
  if (typeof value !== "string") {
    return { ok: true, value };
  }

  const trimmed = value.trim();
  if (!trimmed) {
    return { ok: false };
  }

  try {
    return { ok: true, value: JSON.parse(trimmed) };
  } catch {
    return { ok: false };
  }
}

// Finds the stable key used to group tool-call updates.
function toolBlockKey(update) {
  return (
    update.toolCallId ||
    update.id ||
    update.callId ||
    `tool-${state.toolBlocks.size}`
  );
}

// Extracts tool input parameters from supported ACP tool-call shapes.
function toolParams(update) {
  if (update.rawInput !== undefined) {
    return update.rawInput;
  }
  if (update.input !== undefined) {
    return update.input;
  }
  if (update.arguments !== undefined) {
    return update.arguments;
  }
  return undefined;
}

// Extracts tool output from supported ACP tool-call shapes.
function toolResult(update) {
  if (update.rawOutput !== undefined) {
    return update.rawOutput;
  }
  if (Array.isArray(update.content) && update.content.length > 0) {
    return update.content;
  }
  return undefined;
}

// Formats tool parameters or output for collapsed detail bodies.
function formatToolValue(value) {
  if (Array.isArray(value)) {
    return value.map(formatToolContent).join("");
  }
  if (typeof value === "string") {
    return value;
  }
  return JSON.stringify(value, null, 2);
}

// Formats one ACP tool content item into display text.
function formatToolContent(item) {
  if (!item) {
    return "";
  }
  if (item.type === "content" && item.content) {
    return contentText(item.content.content || item.content);
  }
  if (item.content) {
    return contentText(item.content.content || item.content);
  }
  if (item.type === "diff") {
    return JSON.stringify(item, null, 2);
  }
  return formatToolValue(item);
}

// Adds a system message to the transcript.
function addSystemMessage(text) {
  addMessage("system", text);
}

// Clears transcript state and DOM.
function clearTranscript() {
  resetActiveTextStreams();
  state.toolBlocks.clear();
  resetUsageMeter();
  els.transcript.replaceChildren();
}

// Keeps the transcript pinned to the latest message.
function scrollTranscript() {
  els.transcript.scrollTop = els.transcript.scrollHeight;
}

// Shows a permission request modal and wires option buttons to ACP responses.
function showPermissionRequest(requestId, params) {
  const toolCall = params.toolCall || {};
  state.pendingPermissions.set(requestId, params);
  els.permissionTitle.textContent =
    toolCall.title || toolCall.toolCallId || "Approve action";
  els.permissionBody.textContent = permissionBody(toolCall);
  els.permissionOptions.replaceChildren();

  for (const option of params.options || []) {
    const button = document.createElement("button");
    button.type = "button";
    button.className =
      option.kind && option.kind.startsWith("reject")
        ? "secondary-button"
        : "primary-button";
    button.textContent = option.name || option.optionId;
    button.addEventListener("click", () => {
      resolvePermissionRequest(requestId, {
        outcome: "selected",
        optionId: option.optionId,
      });
    });
    els.permissionOptions.append(button);
  }

  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.className = "secondary-button";
  cancel.textContent = "Cancel";
  cancel.addEventListener("click", () => {
    resolvePermissionRequest(requestId, { outcome: "cancelled" });
  });
  els.permissionOptions.append(cancel);
  els.permissionModal.classList.remove("hidden");
}

// Sends a response for one pending permission request and clears it from UI state.
function resolvePermissionRequest(requestId, outcome) {
  if (!state.pendingPermissions.has(requestId)) {
    return;
  }
  state.pendingPermissions.delete(requestId);
  respondResult(requestId, { outcome });
  if (state.pendingPermissions.size === 0) {
    hidePermissionRequest();
  }
}

// Cancels all pending permission requests before cancelling the running turn.
function cancelPendingPermissions() {
  const requestIds = Array.from(state.pendingPermissions.keys());
  for (const requestId of requestIds) {
    resolvePermissionRequest(requestId, { outcome: "cancelled" });
  }
}

// Drops permission UI state when the transport can no longer send responses.
function clearPermissionRequests() {
  state.pendingPermissions.clear();
  hidePermissionRequest();
}

// Hides the permission request modal.
function hidePermissionRequest() {
  els.permissionModal.classList.add("hidden");
}

// Builds readable permission body text from a tool call update.
function permissionBody(toolCall) {
  if (toolCall.content) {
    return JSON.stringify(toolCall.content, null, 2);
  }
  if (toolCall.rawInput) {
    return JSON.stringify(toolCall.rawInput, null, 2);
  }
  return JSON.stringify(toolCall, null, 2);
}

els.composer.addEventListener("submit", (event) => {
  event.preventDefault();
  submitPrompt();
});

els.promptInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    submitPrompt();
  }
});

els.cancelButton.addEventListener("click", cancelPrompt);
els.newSessionButton.addEventListener("click", createSession);
els.loadMoreSessionsButton.addEventListener("click", () =>
  refreshSessions(state.sessionCursor),
);
els.modelSelect.addEventListener("change", (event) =>
  switchModel(event.target.value),
);
els.modelSelectButton.addEventListener("click", toggleModelMenu);
document.addEventListener("click", (event) => {
  if (!event.target.closest(".composer-model-picker")) {
    closeModelMenu();
  }
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    closeModelMenu();
  }
});

connect();
