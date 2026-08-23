import { dayKey } from "./dates.js";

export function crossesMidnightUtc(startMs, endMs) {
  return dayKey(startMs) !== dayKey(endMs);
}
