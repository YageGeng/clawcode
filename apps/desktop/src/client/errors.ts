/** The message of a thrown value: an `Error`'s message, anything else as text. */
export function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
