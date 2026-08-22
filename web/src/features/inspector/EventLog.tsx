import { memo, useState } from "react";
import type { SessionEvent } from "../../domain/model";

export type EventLogProps = Readonly<{
  events: readonly SessionEvent[];
  diagnostics: readonly string[];
}>;

/** One collapsible ACP event; its payload is serialized only once expanded. */
const EventRow = memo(function EventRow({ event }: Readonly<{ event: SessionEvent }>) {
  const [open, setOpen] = useState(false);
  const title = typeof event.payload.payload === "object" && event.payload.payload !== null && !Array.isArray(event.payload.payload)
    && typeof (event.payload.payload as Record<string, unknown>).event === "string"
    ? (event.payload.payload as Record<string, unknown>).event as string
    : event.type;
  return (
    <details className="event-item" open={open}>
      <summary onClick={(evt) => {
        // Prevent the native toggle and treat it as a controlled switch so the
        // payload is only stringified when the row is actually open.
        evt.preventDefault();
        setOpen((value) => !value);
      }}>
        <span>{title}</span>
        <small>{event.meta === undefined ? "无关联元数据" : `${event.meta.turnId} · ${event.meta.timestampMs}`}</small>
      </summary>
      {open ? <pre>{JSON.stringify(event.payload, null, 2)}</pre> : null}
    </details>
  );
});

/** One collapsible diagnostic message. */
const DiagnosticRow = memo(function DiagnosticRow({ diagnostic }: Readonly<{ diagnostic: string }>) {
  const [open, setOpen] = useState(false);
  return (
    <details className="event-item event-item--diagnostic" open={open}>
      <summary onClick={(evt) => {
        evt.preventDefault();
        setOpen((value) => !value);
      }}><span>diagnostic</span><small>{diagnostic}</small></summary>
      {open ? <pre>{diagnostic}</pre> : null}
    </details>
  );
});

/** Renders the Inspector's event and diagnostic stream with lazy payloads. */
export function EventLog({ events, diagnostics }: EventLogProps) {
  return (
    <div className="event-log">
      {events.length === 0 && diagnostics.length === 0 ? <div className="inspector-empty">当前连接尚未收到事件。</div> : null}
      {events.map((event) => <EventRow key={`${event.type}:${event.order.receivedOrder}`} event={event} />)}
      {diagnostics.map((diagnostic, index) => <DiagnosticRow key={`${diagnostic}:${index}`} diagnostic={diagnostic} />)}
    </div>
  );
}
