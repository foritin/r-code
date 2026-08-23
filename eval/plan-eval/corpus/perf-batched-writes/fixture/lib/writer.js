export function createWriter(sink) {
  return {
    writeEach(records) {
      for (const record of records) sink([record]);
    },
  };
}
