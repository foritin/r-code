import { createWriter } from "./writer.js";

export function runPipeline(records, sink) {
  const writer = createWriter(sink);
  writer.writeEach(records);
}
