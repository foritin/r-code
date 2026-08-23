const lines = [];

export const logger = {
  info(message) { lines.push(message); },
  warn(message) { lines.push(message); },
  all() { return [...lines]; },
};
