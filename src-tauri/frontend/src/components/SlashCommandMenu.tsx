import type { RefObject } from "react";
import type { SlashCommandDefinition, SlashCommandContext } from "../lib/slash-commands";
import {
  CATEGORY_LABELS,
  commandUnavailableReason,
  matchingSlashCommands,
} from "../lib/slash-commands";
import { AnchoredSurface } from "./ui/AnchoredSurface";

interface Props {
  anchorRef: RefObject<HTMLElement | null>;
  value: string;
  context: SlashCommandContext;
  activeIndex: number;
  onActiveIndexChange: (index: number) => void;
  onPick: (command: SlashCommandDefinition) => void;
  onDismiss: () => void;
}

export function SlashCommandMenu({
  anchorRef,
  value,
  context,
  activeIndex,
  onActiveIndexChange,
  onPick,
  onDismiss,
}: Props) {
  const commands = matchingSlashCommands(value, context);
  if (commands.length === 0) return null;

  let lastCategory: SlashCommandDefinition["category"] | null = null;
  return (
    <AnchoredSurface
      id="slash-command-menu"
      anchorRef={anchorRef}
      className="slash-menu"
      role="listbox"
      label="斜杠命令"
      placement="up"
      align="left"
      matchAnchorWidth
      onDismiss={onDismiss}
    >
      <div className="slash-menu-head">
        <span>命令</span>
        <span>↑↓ 选择 · Tab 补全 · Enter 确认</span>
      </div>
      <div className="slash-menu-list">
        {commands.map((command, index) => {
          const showCategory = command.category !== lastCategory;
          lastCategory = command.category;
          const unavailable = commandUnavailableReason(command, context);
          return (
            <div className="slash-menu-entry" key={command.name}>
              {showCategory && <div className="slash-menu-category">{CATEGORY_LABELS[command.category]}</div>}
              <button
                type="button"
                id={`slash-command-option-${index}`}
                role="option"
                aria-selected={index === activeIndex}
                aria-disabled={Boolean(unavailable)}
                data-active={index === activeIndex ? "true" : undefined}
                className="slash-menu-item ring-inset"
                onMouseEnter={() => onActiveIndexChange(index)}
                onMouseDown={(event) => {
                  event.preventDefault();
                  if (!unavailable) onPick(command);
                }}
              >
                <span className="slash-command-name">/{command.name}</span>
                <span className="slash-command-copy">
                  <strong>{command.title}</strong>
                  <small>{unavailable ?? command.description}</small>
                </span>
                {command.argumentHint && <span className="slash-command-args">{command.argumentHint}</span>}
              </button>
            </div>
          );
        })}
      </div>
    </AnchoredSurface>
  );
}
