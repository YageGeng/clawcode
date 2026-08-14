import type { SessionEvent } from "../../domain/model";

export type EventLogProps = Readonly<{
  events: readonly SessionEvent[];
  diagnostics: readonly string[];
}>;

export function EventLog({ events, diagnostics }: EventLogProps) {
  return (
    <div className="event-log">
      {events.length === 0 && diagnostics.length === 0 ? <div className="inspector-empty">当前连接尚未收到事件。</div> : null}
      {events.map((event) => (
        <details className="event-item" key={`${event.type}:${event.order.receivedOrder}`}>
          <summary><span>{typeof event.payload.payload === "object" && event.payload.payload !== null && !Array.isArray(event.payload.payload) && typeof (event.payload.payload as Record<string, unknown>).event === "string" ? (event.payload.payload as Record<string, unknown>).event as string : event.type}</span><small>{event.meta === undefined ? "无关联元数据" : `${event.meta.turnId} · ${event.meta.timestampMs}`}</small></summary>
          <pre>{JSON.stringify(event.payload, null, 2)}</pre>
        </details>
      ))}
      {diagnostics.map((diagnostic, index) => <details className="event-item event-item--diagnostic" key={`${diagnostic}:${index}`}><summary><span>diagnostic</span><small>{diagnostic}</small></summary><pre>{diagnostic}</pre></details>)}
    </div>
  );
}
