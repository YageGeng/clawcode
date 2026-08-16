import type { AvailableCommandEntity } from "../../domain/model";
import { CommandPaletteModel } from "./commandPaletteModel";

export type CommandPaletteProps = Readonly<{
  commands: readonly AvailableCommandEntity[];
  query: string;
  selectedIndex: number;
  onSelect: (command: AvailableCommandEntity) => void;
}>;

/** Renders the accessible command choices supplied by the current ACP Session. */
export function CommandPalette({ commands, query, selectedIndex, onSelect }: CommandPaletteProps) {
  const matches = CommandPaletteModel.matches(commands, query);
  return (
    <div className="command-palette" id="command-palette" role="listbox" aria-label="可用命令">
      {matches.length === 0 ? (
        <div className="command-palette__empty">没有匹配的命令</div>
      ) : matches.map((command, index) => (
        <button
          className="command-palette__option"
          id={`command-option-${index}`}
          type="button"
          role="option"
          aria-selected={index === selectedIndex}
          data-selected={index === selectedIndex}
          key={command.name}
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => onSelect(command)}
        >
          <span className="command-palette__signature">
            <strong>/{command.name}</strong>
            {command.argumentHint === undefined ? null : <code>{command.argumentHint}</code>}
          </span>
          <span className="command-palette__description">{command.description}</span>
        </button>
      ))}
    </div>
  );
}
