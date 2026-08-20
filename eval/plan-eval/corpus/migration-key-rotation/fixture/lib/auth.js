export function verify(store, token, candidate) {
  return store[token] === candidate;
}
