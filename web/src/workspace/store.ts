import { create } from "zustand";

import {
  initialWorkspaceState,
  reduceWorkspace
} from "./state";
import type { WorkspaceAction, WorkspaceState } from "./state";

type WorkspaceStore = WorkspaceState & Readonly<{
  dispatch: (action: WorkspaceAction) => void;
}>;

export const useWorkspaceStore = create<WorkspaceStore>((set) => ({
  ...initialWorkspaceState,
  dispatch: (action) => set((state) => ({
    ...reduceWorkspace(state, action),
    dispatch: state.dispatch
  }))
}));
