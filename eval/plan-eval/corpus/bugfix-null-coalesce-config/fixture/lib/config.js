export const DEFAULTS = {
  server: { host: "0.0.0.0", port: 8080 },
  log: { level: "info" },
};

export function mergeConfig(user) {
  if (!user) return structuredClone(DEFAULTS);
  return { ...DEFAULTS, ...user };
}
