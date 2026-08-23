const store = new Map();

export const cache = {
  get(key) { return store.get(key); },
  set(key, value) { store.set(key + "#", value); },
  size() { return store.size; },
};
