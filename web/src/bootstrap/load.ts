import { UiBootstrapDecoder } from "./model";
import type { UiBootstrap } from "./model";

export async function loadBootstrap(signal: AbortSignal): Promise<UiBootstrap> {
  const response = await fetch("/api/ui/bootstrap", { signal });
  if (!response.ok) {
    throw new Error(`Bootstrap failed with HTTP ${response.status}`);
  }
  return UiBootstrapDecoder.parse(await response.json());
}
