import { useEffect, useState } from "react";

import { loadBootstrap } from "./bootstrap/load";
import type { UiBootstrap } from "./bootstrap/model";
import { AppShell } from "./shell/AppShell";
import { WorkspaceController } from "./workspace/controller";

export function BootstrapApp() {
  const [bootstrap, setBootstrap] = useState<UiBootstrap>();
  const [workspace, setWorkspace] = useState<WorkspaceController>();
  const [error, setError] = useState<string>();

  useEffect(() => {
    const controller = new AbortController();
    let workspace: WorkspaceController | undefined;
    void loadBootstrap(controller.signal)
      .then(async (value) => {
        document.title = value.product.name;
        setBootstrap(value);
        workspace = new WorkspaceController(value);
        setWorkspace(workspace);
        await workspace.start();
      })
      .catch((reason: unknown) => {
        if (!controller.signal.aborted) {
          setError(reason instanceof Error ? reason.message : String(reason));
        }
      });
    return () => {
      controller.abort();
      workspace?.close();
    };
  }, []);

  if (error !== undefined) {
    return <main className="bootstrap-state"><section className="bootstrap-card"><h1>Agent unavailable</h1><p>{error}</p></section></main>;
  }
  if (bootstrap === undefined || workspace === undefined) {
    return <main className="bootstrap-state"><section className="bootstrap-card"><p>Preparing agent workspace…</p></section></main>;
  }
  return <AppShell bootstrap={bootstrap} controller={workspace} />;
}
