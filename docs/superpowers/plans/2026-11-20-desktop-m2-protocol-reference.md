# Desktop M2 protocol reference (clawcode ACP v2)

Companion to [the M2 plan](./2026-11-20-desktop-m2-interactions-and-controls.md). Extracted from the backend (`crates/acp`, `crates/kernel`, `crates/protocol`) and the WebUI (`web/src`, written `W:` below). Wire casing: ACP standard types are camelCase; `_meta.clawcode` product fields are camelCase; `_clawcode/event` payloads and tool-result details are snake_case. u64 counters and timestamps travel as decimal strings (`usage.*`, `contextWindow`, `timestampMs`, `tokensBefore`, `startedAtMs`, `endedAtMs`).

## 0. Connection, capabilities, envelopes

- WebUI initialize (W:workspace/controller.ts:767-777): `capabilities:{elicitation:{form:{},url:{}}}`; handlers for `elicitation/create` and `session/request_permission` (controller.ts:759-760).
- Desktop today (crates/desktop/src/backend.rs `AcpClient::connect`): no capabilities, no request handlers. Consequences: `elicitation/create` is never sent (gated on capabilities, crates/acp/src/elicitation.rs:164-181); plan-approval `session/request_permission` is sent ungated (crates/acp/src/permission.rs:50) and fails method-not-found, leaving the plan pending (permission.rs:107-112).
- Desktop Cargo features today: `unstable_protocol_v2`, `unstable_session_set_title`. Add `unstable_plan_operations` (else `PlanUpdateContent::Markdown` decodes as `Other`) and `unstable_end_turn_token_usage` (else `IdleStateUpdate.usage` is dropped).
- Registering handlers (agent-client-protocol 5.4.0, examples/v2_one_shot_client.rs:53-60, v2_session_coordination.rs:436-444):

```rust
Client.v2().name("clawcode-desktop")
    .on_receive_notification(async move |n: wire::UpdateSessionNotification, _cx| { .. }, agent_client_protocol::on_receive_notification!())
    .on_receive_request(async move |req: wire::RequestPermissionRequest,
        responder: Responder<wire::RequestPermissionResponse>, cx: v2::ConnectionTo<Agent>| { .. },
        agent_client_protocol::on_receive_request!())
    .on_receive_request(async move |req: wire::CreateElicitationRequest,
        responder: Responder<wire::CreateElicitationResponse>, cx| { .. },
        agent_client_protocol::on_receive_request!())
    .connect_with(component, ..)
```

- Handlers must return fast: park the `Responder` and answer later. Cancellation: `responder.cancellation().cancelled().await` (jsonrpc.rs:4790, 2435). On `$/cancel_request` dismiss the prompt and answer `cancelled` (permission) / action `cancel` (elicitation); dropping the responder answers -32800.
- Initialize: `wire::InitializeRequest::new(..).capabilities(wire::ClientCapabilities::new().elicitation(wire::ElicitationCapabilities::new().form(wire::ElicitationFormCapabilities::new()).url(wire::ElicitationUrlCapabilities::new())))`.
- `_clawcode/event` updates arrive as `wire::SessionUpdate::Other(o)` with `o.session_update == "_clawcode/event"` and `o.fields["payload"]` (`{event:"<snake>", ..}`); `_meta.clawcode = {turnId, timestampMs, sequence}` on every update.
- Extension methods (`protocol::AcpExtensionMethod::*.as_str()`), params camelCase with `deny_unknown_fields`: Compact `_clawcode/session/compact {sessionId}`; QuestionDismiss `_clawcode/session/question/dismiss {sessionId, items:[{requestId,index}]}`; others as listed in crates/protocol/src/identity.rs.

## 1. Asynchronous ask-user questions

- Tool `request_user_input_async`, args `{questions:[{title, options?:string[], multi_select?:bool}]}`; limits 1-8 questions, title ≤2048 B, 1-8 options ≤512 B (crates/kernel/src/runtime/question/tool.rs:14-22). Returns at once with text `{"accepted":true}` and details `{type:"user_questions", request_id, questions:[{title, options?, multi_select?}]}`.
- Opening: from the persisted tool result — `tool_call_update.rawOutput.content.details` (W:updateDecoder.ts:192-194, W:domain/model.ts:411-422). Key `{requestId, index}`, id string `${requestId}:${index}`.
- Settlement: `_clawcode/event` payload `{event:"message_end", message:{content:{type:"extension", extension:{custom_type:"question_reply", details:{items:[{request_id, index, question, answer?}]}}}}}`; no `answer` = skipped. Also queued steering/follow-up items carrying a reply (W:model.ts:459 `Questions.queued`), and the client's in-flight settling set.
- Open-question rule (W:selectors.ts:50-70): asked, not settled by a reply record, not carried by a queued item, not in-flight.
- UI (W:features/composer/QuestionPanel.tsx): panel above the composer, one question at a time, header "问题 · 第 i 题 / 共 n 题" with prev/next; title as Markdown; "可多选" hint; radio or checkbox options, first preselected but not sent until 发送; extra "其他" choice with a 2-row textarea (only choice when no options); multi answer = `picks.map(p => "- " + p).join("\n")`, free text last; 发送 disabled when blank; 跳过 dismisses; never steals focus.
- Answer: `session/prompt {sessionId, prompt:[{type:"text", text}], _meta:{clawcode:{submissionId, questionReply:{requestId, index}}}}` — exactly one non-blank text block, no `whenBusy`; steers or starts a Run. Error -32602 `data.reason == "questionClosed"` → notice "该问题已在其他地方回答或跳过".
- Dismiss failure → notice "未能跳过待回答的问题，请重试". A normal prompt supersedes open questions (kernel skips them). On going idle, the in-flight set resets.

## 2. Plan mode and plan approval

- Plan mode is the `mode` config option (`default` | `plan`), set via `session/set_config_option` (§6). WebUI composer badge "Plan · 只读" when `mode == plan`.
- Tool `exit_plan_mode {plan}` (kind `switch_mode`) → `session/request_permission`:

```
{sessionId, title:"Ready to execute the plan?",
 options:[{optionId:"approve",name:"Approve and execute",kind:"allow_once"},
          {optionId:"keep_planning",name:"Keep planning",kind:"reject_once"}],
 subject:{type:"tool_call", toolCall:{toolCallId, kind:"switch_mode",
          content:[{type:"content", content:{type:"text", text:<plan markdown>}}]}}}
```

- Response `{outcome:{outcome:"selected", optionId}}` or `{outcome:{outcome:"cancelled"}}`. Re-sent on resume; resolved elsewhere → `$/cancel_request`. `session/cancel` resolves it `cancelled`.
- WebUI dialog (W:features/session/PermissionDialog.tsx): "执行这个计划？", plan Markdown, buttons 继续规划 (secondary, default focus) and 批准并执行 (primary). Desktop spec §7.3 docks it as a card with 批准 / 继续规划.
- `plan_update` (crates/acp/src/plan.rs): todos `{plan:{type:"items", planId:"todos", entries:[{content, priority, status: pending|in_progress|completed}]}}`; approved plan `{plan:{type:"markdown", planId:"plan-mode", content}}`. WebUI panel (W:features/composer/PlanPanel.tsx): collapsible "计划" with "n/m", per-item icons (check / spinner / circle), nested "已批准的计划"; open while a run that published plan content runs, collapses at run end, reopens on new content.

## 3. Permissions

- Only plan approvals are sent today. Generic handling: `title`, `description?`, `options[].{optionId, name, kind}`; reply selected or cancelled. Rust: `wire::RequestPermissionResponse::new(RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)))`; schema agent-client-protocol-schema-1.19.0/src/v2/client.rs:3848-4480.

## 4. MCP elicitation

- Requires initialize `elicitation.form` / `.url`. Request `elicitation/create`: `message`, `_meta.clawcode.mcpServerId`, scope `sessionId` (+ `toolCallId?`) XOR `requestId` (request-scoped = login, M4).
  - form: `{mode:"form", requestedSchema:{type:"object", properties, required?}}`
  - url: `{mode:"url", elicitationId, url}`
- Response `{action:"accept", content?}` | `{action:"decline"}` | `{action:"cancel"}`; content values string | number | integer | boolean | string[]. Server validates against the schema; violations become cancel.
- Form fields (W:features/mcp/elicitationForm.ts): string (minLength, maxLength, pattern, format email|uri|date|date-time, default); string enum / oneOf[{const,title}] → select; number/integer (min, max, default); boolean → checkbox (default false); array of enum / anyOf → multi-checkbox (minItems, maxItems). Label `title ?? name`, " *" when required; required empty → "必填"; lengths in code points; empty optional fields omitted. Unrenderable required field → reject.
- Dialog: "{serverId} 请求输入" (or "MCP 请求输入"); buttons 取消 (cancel, Esc, default focus), 拒绝 (decline), 提交 (accept). URL mode: "打开并继续" opens the URL and accepts without content; shows "将打开 <host>".

## 5. Slash commands

- `available_commands_update {availableCommands:[{name, description, input?:{hint}, _meta:{clawcode:{slashCommand:{source: builtin|extension|skill|prompt_template, qualifiedName?, aliasKind: canonical|short}}}}]}` — full snapshot each time; `name` has no slash.
- Palette (W:features/composer/Composer.tsx, commandPaletteModel.ts): opens when text starts with "/" and has no whitespace; prefix match on name (case-insensitive) in snapshot order; ↑/↓ wrap; Esc dismisses until the next edit; Tab or Enter selects; selection writes `"/" + name + (hint ? " " : "")` and does NOT send; rows show `/name`, hint, source badge (Builtin / Extension / Skill / Template), description, qualified name; "没有匹配的命令".
- Sending: plain text prompt; the kernel parses `/name args`.
- Messages: invocation `user_message` with `_meta.clawcode.slashCommand {name, source, messageKind:"invocation"}`; output `agent_message` with `{name, source, status: succeeded|failed|cancelled, messageKind:"output"}`; skill/template expansion `messageKind:"expansion"`. Card (W:features/conversation/CommandMessageCard.tsx): `/name`, source badge, kind label (调用 / 结果 / 展开调用), status (成功 / 失败 / 已取消), Markdown body with the leading `/name` stripped for non-output kinds.

## 6. Model, thinking and mode options

- `config_option_update {configOptions:[..]}`; also in `session/resume`, `session/new` and `session/set_config_option` responses. Order [model, thinking_level, mode], each `{type:"select", configId, name, category, currentValue, options:[{value, name, description?}]}`.
  - model: values `"<provider>/<model>"`, option name = profile display name.
  - thinking_level: `off, minimal, low, medium, high, x_high` (Off … Extra high).
  - mode: `default, plan`.
- Set: `session/set_config_option {sessionId, configId, type:"id", value}`; Rust `wire::SetSessionConfigOptionRequest::new(sid, "model", wire::SessionConfigOptionValue::id(..))`; response carries the full new list. Errors -32602 (unknown id/value), -32000 with `data.methodId` (model needs sign-in → M4 login; M2 shows a notice).

## 7. Context usage and run usage

- `usage_update {used, size, _meta:{clawcode:{usage:{input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, total_tokens}, contextWindow}}}`; fallback `_clawcode/event {event:"usage_updated", usage, context_window}`. Counters are decimal strings; drop the whole usage when one is malformed.
- `used` = `total_tokens` if present else `input + output` (NOT `update.used`); `size` = `contextWindow ?? update.size`; percent = `min(1000, used*1000/size)/10`, "--" when unknown; warning tone ≥ 85%. Cleared after a completed compaction.
- Per message: `_meta.clawcode.assistant = {provider_id, model_id, stop_reason, error?, usage:{..}}` on `agent_message`; `messageTiming`.
- Per run: the WebUI sums `assistant.usage` of the run's messages; cache hit = `cache_read*1000/input` per-mille, capped 100%, unknown if input or cache_read missing or input is 0. The WebUI has no compact usage row; the desktop row (spec §7.2) is new.

## 8. Compaction and branch summary

- `compaction_update {compactionId, status: in_progress|completed|failed|cancelled, summary?:[ContentBlock], error?}` with `_meta.clawcode.compaction`: start `{reason: manual|threshold|overflow}`; end `{reason, result: null | {entryId, turnId, tokensBefore, usage?, startedAtMs, endedAtMs}}`.
- WebUI: running "正在压缩上下文…"; completed → transcript row keyed by `entryId` (collapsed "上下文已压缩 / 从 N tokens 压缩", expands to reason · elapsed and the Markdown summary; reasons 手动 / 达到上下文阈值 / 上下文溢出恢复); failed "上下文压缩失败：…"; cancelled "已取消上下文压缩".
- Manual: `/compact` (builtin) or `_clawcode/session/compact {sessionId}`.
- Branch summary: `user_message` with `_meta.clawcode.origin:"branch_summary"`, rendered as a collapsed card "分支摘要 / 已离开分支的摘要".

## 9. Image attachments

- Prompt block `{type:"image", data:<standard base64, no data-URL prefix>, mimeType}` after the text block; MIME png|jpeg|gif|webp; ≤5 images, ≤10 MiB each, ≤20 MiB total (server rechecks, crates/acp/src/input.rs:7-100, -32602 on violation).
- The server always accepts images; text-only models get placeholders at request time, and the client cannot see model modalities.
- WebUI: file picker + paste (no drag-and-drop); "不支持图片格式"; thumbnails with remove; send disabled while reading.

## 10. Notices, retries, errors

- `notice {severity, title, description?}` — live-only, global; WebUI toast auto-dismisses after 6 s.
- Retry `_clawcode/event`: `retry_scheduled {attempt, max_attempts, delay_ms, error}` (countdown from `timestampMs + delay_ms`), `retry_start {attempt, max_attempts}`, `retry_end {attempt, success, final_error?}`; cleared when idle except a failed finish.
- `extension_handler_failed {extension_id, point, message}` → inline error card keyed `error:<turnId>:<sequence>`; persisted `handler_error` extension messages likewise.
- `assistant.error` → inline red "模型调用失败 <error>" in the agent message; empty messages with an error stay visible.
