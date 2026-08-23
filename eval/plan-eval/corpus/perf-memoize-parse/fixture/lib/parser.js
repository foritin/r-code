export function parseLine(line) {
  const parts = line.split("=>").map((part) => part.trim());
  return { key: parts[0], value: parts[1] ?? null };
}
