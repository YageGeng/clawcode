/** Where the app is: a host, and the open Session on it, if any. */
export type AppLocation = Readonly<{ hostId: string; sessionId: string | null }>;

/** The most Locations the history keeps; older ones fall off the start. */
const MAX_ENTRIES = 100;

/** Back/forward history of Locations, newest last; at most 100 entries. */
export class NavigationHistory {
  private readonly entries: AppLocation[];
  private index = 0;

  /** Starts the history at `initial`. */
  constructor(initial: AppLocation) {
    this.entries = [initial];
  }

  /** The Location shown now. */
  get current(): AppLocation {
    return this.entries[this.index]!;
  }

  /** Whether `back` would move. */
  get canGoBack(): boolean {
    return this.index > 0;
  }

  /** Whether `forward` would move. */
  get canGoForward(): boolean {
    return this.index < this.entries.length - 1;
  }

  /** Moves to `location`, dropping forward entries; a no-op when it equals `current`. */
  push(location: AppLocation): void {
    if (sameLocation(location, this.current)) return;
    this.entries.splice(this.index + 1);
    this.entries.push(location);
    if (this.entries.length > MAX_ENTRIES) this.entries.shift();
    this.index = this.entries.length - 1;
  }

  /** Steps back; the new current, or `undefined` at the start. */
  back(): AppLocation | undefined {
    if (!this.canGoBack) return undefined;
    this.index -= 1;
    return this.current;
  }

  /** Steps forward; the new current, or `undefined` at the end. */
  forward(): AppLocation | undefined {
    if (!this.canGoForward) return undefined;
    this.index += 1;
    return this.current;
  }
}

/** Whether two Locations name the same host and Session. */
function sameLocation(left: AppLocation, right: AppLocation): boolean {
  return left.hostId === right.hostId && left.sessionId === right.sessionId;
}
