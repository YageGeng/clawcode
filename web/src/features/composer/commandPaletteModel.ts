import type { AvailableCommandEntity } from "../../domain/model";

export const CommandPaletteModel = {
  /** Filters one backend command snapshot using pi-compatible prefix discovery. */
  matches(
    commands: readonly AvailableCommandEntity[],
    query: string
  ): readonly AvailableCommandEntity[] {
    const normalized = query.toLocaleLowerCase();
    return commands.filter((command) => command.name.toLocaleLowerCase().startsWith(normalized));
  }
} as const;
