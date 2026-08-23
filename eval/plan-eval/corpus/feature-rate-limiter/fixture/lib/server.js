export function makeServer() {
  const routes = new Map();
  return {
    on(path, handler) { routes.set(path, handler); },
    async handleRequest(path, key, payload) {
      const handler = routes.get(path);
      if (!handler) return { status: 404 };
      return handler(payload, key);
    },
  };
}
