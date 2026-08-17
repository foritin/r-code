import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { renderIconPng, renderSmallIcon } from "./manual/render-small-app-icons.mjs";

const repoRoot = new URL("../", import.meta.url);

function pixel(image, x, y) {
  const offset = (y * image.size + x) * 4;
  return image.rgba.subarray(offset, offset + 4);
}

function icoFrames(buffer) {
  assert.equal(buffer.readUInt16LE(0), 0);
  assert.equal(buffer.readUInt16LE(2), 1);
  const count = buffer.readUInt16LE(4);
  const frames = new Map();
  for (let index = 0; index < count; index += 1) {
    const entry = 6 + index * 16;
    const width = buffer[entry] || 256;
    const height = buffer[entry + 1] || 256;
    const length = buffer.readUInt32LE(entry + 8);
    const offset = buffer.readUInt32LE(entry + 12);
    assert.equal(width, height);
    frames.set(width, buffer.subarray(offset, offset + length));
  }
  return frames;
}

test("small app icons use a simplified, high-contrast mark", () => {
  for (const size of [16, 20, 24, 32]) {
    const image = renderSmallIcon(size);
    assert.equal(image.rgba.length, size * size * 4);
    assert.equal(pixel(image, 0, 0)[3], 0, `${size}px corner must stay transparent`);

    const topCenter = pixel(image, Math.floor(size / 2), 1);
    assert.ok(topCenter[0] > 200 && topCenter[1] > 70, `${size}px top edge must remain orange`);

    let darkPixels = 0;
    for (let offset = 0; offset < image.rgba.length; offset += 4) {
      if (image.rgba[offset + 3] > 200 && image.rgba[offset] < 70) darkPixels += 1;
    }
    assert.ok(darkPixels >= size * size * 0.12, `${size}px R mark is too small`);
    assert.ok(darkPixels <= size * size * 0.38, `${size}px R mark is too crowded`);
  }

  const plain = renderSmallIcon(24);
  const bracketed = renderSmallIcon(32);
  const plainLeft = pixel(plain, Math.round(plain.size * 0.13), Math.round(plain.size * 0.5));
  const bracketLeft = pixel(
    bracketed,
    Math.round(bracketed.size * 0.13),
    Math.round(bracketed.size * 0.5),
  );
  assert.ok(plainLeft[0] > 180, "24px icon should omit code brackets");
  assert.ok(bracketLeft[0] < 70, "32px icon should restore simplified code brackets");
});

test("generated PNG payloads are valid RGBA images", () => {
  for (const size of [16, 20, 24, 32]) {
    const png = renderIconPng(size);
    assert.deepEqual(png.subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
    assert.equal(png.readUInt32BE(16), size);
    assert.equal(png.readUInt32BE(20), size);
    assert.equal(png[24], 8);
    assert.equal(png[25], 6);
  }
});

test("checked-in Windows ICO contains the exact specialized small frames", async () => {
  const ico = await readFile(new URL("icons/icon.ico", repoRoot));
  const frames = icoFrames(ico);
  assert.deepEqual([...frames.keys()], [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]);
  for (const size of [16, 20, 24, 32]) {
    assert.deepEqual(frames.get(size), renderIconPng(size), `${size}px ICO frame is stale`);
  }

  const linuxSmall = await readFile(new URL("icons/32x32.png", repoRoot));
  assert.deepEqual(linuxSmall, renderIconPng(32));
});
