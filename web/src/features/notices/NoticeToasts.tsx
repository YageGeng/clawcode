import { TriangleAlert, X } from "lucide-react";
import { useEffect } from "react";

import type { SessionNotice } from "../../domain/model";
import { useWorkspaceStore } from "../../workspace/store";

const NOTICE_LIFETIME_MS = 6_000;

/** Shows one warning toast that dismisses itself after six seconds. */
function NoticeToast({ notice }: Readonly<{ notice: SessionNotice }>) {
  const dispatch = useWorkspaceStore((state) => state.dispatch);
  useEffect(() => {
    const timer = window.setTimeout(() => dispatch({ type: "notice/dismissed", id: notice.id }), NOTICE_LIFETIME_MS);
    return () => window.clearTimeout(timer);
  }, [dispatch, notice.id]);
  return (
    <div className="notice-toast" role="status">
      <TriangleAlert className="notice-toast__icon" size={16} aria-hidden="true" />
      <div className="notice-toast__body">
        <strong>{notice.title}</strong>
        {notice.description === undefined ? null : <p>{notice.description}</p>}
      </div>
      <button className="icon-button" type="button" aria-label="关闭提示" title="关闭提示" onClick={() => dispatch({ type: "notice/dismissed", id: notice.id })}><X size={14} aria-hidden="true" /></button>
    </div>
  );
}

/** Stacks the live Session warnings the agent reports; they never replay. */
export function NoticeToasts() {
  const notices = useWorkspaceStore((state) => state.notices);
  if (notices.length === 0) return null;
  return <div className="notice-toasts" aria-live="polite">{notices.map((notice) => <NoticeToast key={notice.id} notice={notice} />)}</div>;
}
