import { parseLine } from "./parser.js";

export function parseAll(lines) {
  return lines.map((line) => parseLine(line));
}
