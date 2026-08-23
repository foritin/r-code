import { mergeConfig } from "./config.js";

export function servicePort(user) {
  const config = mergeConfig(user);
  return config.server.port;
}
