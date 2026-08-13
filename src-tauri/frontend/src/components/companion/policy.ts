export type CompanionMood = "idle" | "working" | "attention" | "success" | "error" | "review";

const AUDIBLE_MOODS: ReadonlySet<CompanionMood> = new Set([
  "attention",
  "success",
  "error",
  "review",
]);

export function shouldPlayCompanionCue(
  previous: CompanionMood | null,
  next: CompanionMood,
): boolean {
  if (previous === null || previous === next || !AUDIBLE_MOODS.has(next)) return false;
  // A normal reviewed task is shown as success while its completion toast is visible, then
  // settles on review when that toast expires. Both states describe the same completion event.
  return !(previous === "success" && next === "review");
}
