export async function retry(fn, attempts = 3) {
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      fn();
      return true;
    } catch (error) {
      if (attempt === attempts) throw error;
    }
  }
}
