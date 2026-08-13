#!/usr/bin/env python3
"""Fail when the shipped companion atlas can bleed, clip, or jitter between cells."""

from __future__ import annotations

import argparse
from collections import deque
import json
from pathlib import Path

from PIL import Image

CELL_WIDTH = 192
CELL_HEIGHT = 208
LAYOUTS = {
    (CELL_WIDTH * 8, CELL_HEIGHT * 9): ([6, 8, 8, 4, 5, 8, 6, 6, 6], {0, 3, 7}),
    (CELL_WIDTH * 8, CELL_HEIGHT * 2): ([8, 8], {0, 1}),
}


def edge_alpha(cell: Image.Image, margin: int) -> int:
    alpha = cell.getchannel("A")
    width, height = alpha.size
    return sum(
        sum(alpha.crop(box).histogram()[1:])
        for box in (
            (0, 0, width, margin),
            (0, height - margin, width, height),
            (0, 0, margin, height),
            (width - margin, 0, width, height),
        )
    )


def lower_center(cell: Image.Image) -> float | None:
    alpha = cell.getchannel("A")
    bbox = alpha.getbbox()
    if bbox is None:
        return None
    top, bottom = bbox[1], bbox[3]
    threshold = max(top + (bottom - top) * 0.72, bottom - 34)
    points = [
        (x, y)
        for y in range(int(threshold), cell.height)
        for x in range(cell.width)
        if alpha.getpixel((x, y)) > 16
    ]
    return sum(x for x, _ in points) / len(points) if points else None


def detached_components(cell: Image.Image, threshold: int, min_pixels: int) -> list[int]:
    alpha = cell.getchannel("A")
    width, height = alpha.size
    visible = alpha.load()
    visited: set[tuple[int, int]] = set()
    sizes: list[int] = []
    for y in range(height):
        for x in range(width):
            if visible[x, y] <= threshold or (x, y) in visited:
                continue
            queue = deque([(x, y)])
            visited.add((x, y))
            size = 0
            while queue:
                current_x, current_y = queue.popleft()
                size += 1
                for next_x in range(current_x - 1, current_x + 2):
                    for next_y in range(current_y - 1, current_y + 2):
                        point = (next_x, next_y)
                        if (
                            0 <= next_x < width
                            and 0 <= next_y < height
                            and point not in visited
                            and visible[next_x, next_y] > threshold
                        ):
                            visited.add(point)
                            queue.append(point)
            if size >= min_pixels:
                sizes.append(size)
    return sorted(sizes, reverse=True)[1:]


def audit(
    path: Path,
    edge_margin: int,
    max_anchor_drift: float,
    component_threshold: int,
    min_detached_pixels: int,
) -> dict[str, object]:
    atlas = Image.open(path).convert("RGBA")
    errors: list[str] = []
    cells: list[dict[str, object]] = []
    layout = LAYOUTS.get(atlas.size)
    if layout is None:
        expected = " or ".join(f"{width}x{height}" for width, height in LAYOUTS)
        errors.append(f"expected {expected}, got {atlas.width}x{atlas.height}")
        return {"ok": False, "file": str(path), "errors": errors, "cells": cells}
    required_frames, stable_rows = layout

    for row, count in enumerate(required_frames):
        anchors: list[float] = []
        for column in range(count):
            cell = atlas.crop((
                column * CELL_WIDTH,
                row * CELL_HEIGHT,
                (column + 1) * CELL_WIDTH,
                (row + 1) * CELL_HEIGHT,
            ))
            bbox = cell.getchannel("A").getbbox()
            edges = edge_alpha(cell, edge_margin)
            anchor = lower_center(cell)
            detached = detached_components(cell, component_threshold, min_detached_pixels)
            if bbox is None:
                errors.append(f"row {row} column {column} is empty")
            if edges:
                errors.append(f"row {row} column {column} has {edges} visible safe-edge pixels")
            if detached:
                errors.append(
                    f"row {row} column {column} has detached visible components: {detached} pixels"
                )
            if anchor is not None:
                anchors.append(anchor)
            cells.append({
                "row": row,
                "column": column,
                "bbox": bbox,
                "edge_pixels": edges,
                "lower_center_x": anchor,
                "detached_components": detached,
            })
        if row in stable_rows and anchors and max(anchors) - min(anchors) > max_anchor_drift:
            errors.append(
                f"row {row} lower-body anchor drifts {max(anchors) - min(anchors):.2f}px "
                f"(limit {max_anchor_drift:.2f}px)"
            )
    return {"ok": not errors, "file": str(path), "errors": errors, "cells": cells}


def audit_sequence_silhouette(path: Path, row: int, frames: list[int], max_width_delta: int) -> list[str]:
    atlas = Image.open(path).convert("RGBA")
    widths: list[int] = []
    centers: list[float] = []
    for column in frames:
        alpha = atlas.crop((
            column * CELL_WIDTH,
            row * CELL_HEIGHT,
            (column + 1) * CELL_WIDTH,
            (row + 1) * CELL_HEIGHT,
        )).getchannel("A")
        bbox = alpha.getbbox()
        if bbox is None:
            return [f"hover sequence row {row} column {column} is empty"]
        widths.append(bbox[2] - bbox[0])
        centers.append((bbox[0] + bbox[2]) / 2)
    errors: list[str] = []
    if max(widths) - min(widths) > max_width_delta:
        errors.append(
            f"hover silhouette width changes {max(widths) - min(widths)}px "
            f"(limit {max_width_delta}px)"
        )
    if max(centers) - min(centers) > 3:
        errors.append(f"hover silhouette center drifts {max(centers) - min(centers):.2f}px")
    return errors


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("atlas")
    parser.add_argument("--json-out")
    parser.add_argument("--edge-margin", type=int, default=4)
    parser.add_argument("--max-anchor-drift", type=float, default=4.0)
    parser.add_argument("--component-threshold", type=int, default=16)
    parser.add_argument("--min-detached-pixels", type=int, default=30)
    args = parser.parse_args()
    result = audit(
        Path(args.atlas).expanduser().resolve(),
        args.edge_margin,
        args.max_anchor_drift,
        args.component_threshold,
        args.min_detached_pixels,
    )
    if Path(args.atlas).name == "r-code-miku-v4.webp":
        result["errors"].extend(audit_sequence_silhouette(
            Path(args.atlas).expanduser().resolve(),
            0,
            [0, 1, 2, 3, 4, 5, 4, 2],
            8,
        ))
        result["ok"] = not result["errors"]
    if args.json_out:
        target = Path(args.json_out).expanduser().resolve()
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: value for key, value in result.items() if key != "cells"}, indent=2))
    raise SystemExit(0 if result["ok"] else 1)


if __name__ == "__main__":
    main()
