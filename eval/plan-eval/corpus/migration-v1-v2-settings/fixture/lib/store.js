export function loadSettings(raw) {
  return typeof raw === "string" ? JSON.parse(raw) : raw;
}
