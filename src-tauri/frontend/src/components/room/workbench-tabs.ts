import type { KeyboardEvent } from "react";

/** WAI-ARIA tabs keyboard navigation shared by tool and subagent workbench tabs. */
export function handleWorkbenchTabListKeyDown(event: KeyboardEvent<HTMLElement>) {
  if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
  const target = event.target instanceof HTMLElement
    ? event.target.closest<HTMLElement>("[role='tab']")
    : null;
  if (!target) return;

  const tabs = Array.from(
    event.currentTarget.querySelectorAll<HTMLElement>("[role='tab']:not([disabled])"),
  );
  const current = tabs.indexOf(target);
  if (current < 0 || tabs.length < 2) return;

  event.preventDefault();
  const next = event.key === "Home"
    ? 0
    : event.key === "End"
      ? tabs.length - 1
      : (current + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
  tabs[next]?.focus();
  tabs[next]?.click();
}
