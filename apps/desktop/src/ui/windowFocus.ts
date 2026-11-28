import { focusedWindow } from "nsh/desktop";

/**
 * Whether the main window has the keyboard focus (or no window of the app does). The main UI is the app root, whose
 * key bindings fire in every window, so a binding for the main window is made only while this holds; reactive.
 */
export function mainWindowFocused(): boolean {
  const focused = focusedWindow();
  return focused === null || focused.main;
}
