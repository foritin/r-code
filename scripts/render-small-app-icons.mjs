#!/usr/bin/env node

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { deflateSync } from "node:zlib";

const DEFAULT_SIZES = [16, 20, 24, 32];
const SAMPLE_GRID = 4;

const R_OUTLINE = buildPolygon([
  ["M", 0.23, 0.16],
  ["L", 0.56, 0.16],
  ["C", 0.75, 0.16, 0.85, 0.26, 0.85, 0.41],
  ["C", 0.85, 0.53, 0.79, 0.61, 0.68, 0.65],
  ["L", 0.86, 0.84],
  ["L", 0.66, 0.84],
  ["L", 0.49, 0.67],
  ["L", 0.44, 0.67],
  ["L", 0.44, 0.84],
  ["L", 0.23, 0.84],
  ["Z"],
]);

function buildPolygon(commands) {
  const points = [];
  let current = [0, 0];
  let start = [0, 0];

  for (const command of commands) {
    const [kind, ...values] = command;
    if (kind === "M") {
      current = [values[0], values[1]];
      start = current;
      points.push(current);
    } else if (kind === "L") {
      current = [values[0], values[1]];
      points.push(current);
    } else if (kind === "C") {
      const [cx1, cy1, cx2, cy2, x, y] = values;
      const [x0, y0] = current;
      for (let step = 1; step <= 16; step += 1) {
        const t = step / 16;
        const u = 1 - t;
        points.push([
          u ** 3 * x0 + 3 * u ** 2 * t * cx1 + 3 * u * t ** 2 * cx2 + t ** 3 * x,
          u ** 3 * y0 + 3 * u ** 2 * t * cy1 + 3 * u * t ** 2 * cy2 + t ** 3 * y,
        ]);
      }
      current = [x, y];
    } else if (kind === "Z") {
      points.push(start);
      current = start;
    } else {
      throw new Error(`Unsupported path command: ${kind}`);
    }
  }

  return points;
}

function pointInPolygon(x, y, polygon) {
  let inside = false;
  for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i, i += 1) {
    const [xi, yi] = polygon[i];
    const [xj, yj] = polygon[j];
    const intersects = yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi;
    if (intersects) inside = !inside;
  }
  return inside;
}

function insideRoundedRect(x, y, left, top, right, bottom, radius) {
  if (x < left || x > right || y < top || y > bottom) return false;
  const cx = Math.max(left + radius, Math.min(x, right - radius));
  const cy = Math.max(top + radius, Math.min(y, bottom - radius));
  return (x - cx) ** 2 + (y - cy) ** 2 <= radius ** 2;
}

function distanceToSegment(x, y, ax, ay, bx, by) {
  const dx = bx - ax;
  const dy = by - ay;
  const lengthSquared = dx * dx + dy * dy;
  const t = lengthSquared === 0 ? 0 : Math.max(0, Math.min(1, ((x - ax) * dx + (y - ay) * dy) / lengthSquared));
  return Math.hypot(x - (ax + t * dx), y - (ay + t * dy));
}

function insideChevron(x, y, direction) {
  const centerX = direction === "left" ? 0.13 : 0.87;
  const outerX = direction === "left" ? 0.2 : 0.8;
  const stroke = 0.045;
  return (
    distanceToSegment(x, y, outerX, 0.38, centerX, 0.5) <= stroke ||
    distanceToSegment(x, y, centerX, 0.5, outerX, 0.62) <= stroke
  );
}

function insideRMark(x, y, withChevrons) {
  const horizontalScale = withChevrons ? 0.72 : 1;
  const sourceX = 0.5 + (x - 0.5) / horizontalScale;
  const inOutline = pointInPolygon(sourceX, y, R_OUTLINE);
  const inCounter = insideRoundedRect(sourceX, y, 0.43, 0.31, 0.68, 0.51, 0.085);
  return inOutline && !inCounter;
}

function backgroundColor(x, y) {
  const mix = Math.max(0, Math.min(1, y * 0.9 + x * 0.1));
  return [
    255,
    Math.round(174 + (118 - 174) * mix),
    Math.round(30 + (24 - 30) * mix),
  ];
}

export function renderSmallIcon(size) {
  if (!Number.isInteger(size) || size < 16 || size > 32) {
    throw new Error(`Small icon size must be an integer from 16 through 32, got ${size}`);
  }

  const rgba = Buffer.alloc(size * size * 4);
  const withChevrons = size >= 32;
  const samples = SAMPLE_GRID * SAMPLE_GRID;

  for (let py = 0; py < size; py += 1) {
    for (let px = 0; px < size; px += 1) {
      let occupied = 0;
      let red = 0;
      let green = 0;
      let blue = 0;

      for (let sy = 0; sy < SAMPLE_GRID; sy += 1) {
        for (let sx = 0; sx < SAMPLE_GRID; sx += 1) {
          const x = (px + (sx + 0.5) / SAMPLE_GRID) / size;
          const y = (py + (sy + 0.5) / SAMPLE_GRID) / size;
          const inPlate = insideRoundedRect(x, y, 0.015, 0.015, 0.985, 0.985, 0.205);
          if (!inPlate) continue;

          occupied += 1;
          const inMark =
            insideRMark(x, y, withChevrons) ||
            (withChevrons && (insideChevron(x, y, "left") || insideChevron(x, y, "right")));
          const [r, g, b] = inMark ? [17, 20, 22] : backgroundColor(x, y);
          red += r;
          green += g;
          blue += b;
        }
      }

      const offset = (py * size + px) * 4;
      if (occupied === 0) continue;
      rgba[offset] = Math.round(red / occupied);
      rgba[offset + 1] = Math.round(green / occupied);
      rgba[offset + 2] = Math.round(blue / occupied);
      rgba[offset + 3] = Math.round((occupied / samples) * 255);
    }
  }

  return { size, rgba };
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let value = n;
    for (let bit = 0; bit < 8; bit += 1) {
      value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
    }
    table[n] = value >>> 0;
  }
  return table;
})();

function crc32(buffer) {
  let crc = 0xffffffff;
  for (const value of buffer) crc = CRC_TABLE[(crc ^ value) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const typeBuffer = Buffer.from(type, "ascii");
  const payload = Buffer.concat([typeBuffer, data]);
  const output = Buffer.alloc(data.length + 12);
  output.writeUInt32BE(data.length, 0);
  typeBuffer.copy(output, 4);
  data.copy(output, 8);
  output.writeUInt32BE(crc32(payload), data.length + 8);
  return output;
}

export function encodePng({ size, rgba }) {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(size, 0);
  header.writeUInt32BE(size, 4);
  header[8] = 8;
  header[9] = 6;

  const rows = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y += 1) {
    const rowOffset = y * (size * 4 + 1);
    rows[rowOffset] = 0;
    rgba.copy(rows, rowOffset + 1, y * size * 4, (y + 1) * size * 4);
  }

  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    pngChunk("IHDR", header),
    pngChunk("IDAT", deflateSync(rows, { level: 9 })),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

export function renderIconPng(size) {
  return encodePng(renderSmallIcon(size));
}

function parseArguments(argv) {
  let output = null;
  let sizes = DEFAULT_SIZES;
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === "--output") output = argv[++index];
    else if (argv[index] === "--sizes") {
      sizes = argv[++index].split(",").map((value) => Number.parseInt(value, 10));
    } else throw new Error(`Unknown argument: ${argv[index]}`);
  }
  if (!output) throw new Error("--output is required");
  return { output: resolve(output), sizes };
}

function main() {
  const { output, sizes } = parseArguments(process.argv.slice(2));
  mkdirSync(output, { recursive: true });
  for (const size of sizes) {
    const target = join(output, `${size}x${size}.png`);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, renderIconPng(size));
  }
  process.stdout.write(`Rendered pixel-aligned R-Code icons: ${sizes.join(", ")}px\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main();
