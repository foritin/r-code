#!/usr/bin/env python3
"""Re-encode the companion atlas with lossy RGB and byte-exact alpha."""

from __future__ import annotations

import argparse
import math
import os
from pathlib import Path

from PIL import Image, ImageChops


DEFAULT_ATLAS = (
    Path(__file__).resolve().parents[1]
    / "src"
    / "assets"
    / "companion"
    / "r-code-miku-v4.webp"
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--atlas", type=Path, default=DEFAULT_ATLAS)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--quality", type=int, default=92)
    return parser.parse_args()


def composite_psnr(original: Image.Image, encoded: Image.Image, color: int) -> float:
    background = Image.new("RGBA", original.size, (color, color, color, 255))
    original_rgb = Image.alpha_composite(background, original).convert("RGB")
    encoded_rgb = Image.alpha_composite(background, encoded).convert("RGB")
    histogram = ImageChops.difference(original_rgb, encoded_rgb).histogram()
    squared_error = sum((index % 256) ** 2 * count for index, count in enumerate(histogram))
    samples = original.width * original.height * 3
    mse = squared_error / samples
    return math.inf if mse == 0 else 10 * math.log10((255 * 255) / mse)


def main() -> None:
    args = parse_args()
    if not 1 <= args.quality <= 100:
        raise SystemExit("--quality must be between 1 and 100")

    source = args.atlas.expanduser().resolve()
    destination = (args.output or source).expanduser().resolve()
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(f".{destination.name}.tmp.webp")

    original = Image.open(source).convert("RGBA")
    original_size = source.stat().st_size
    original.save(
        temporary,
        "WEBP",
        lossless=False,
        quality=args.quality,
        method=6,
        exact=True,
        alpha_quality=100,
    )
    encoded = Image.open(temporary).convert("RGBA")
    if encoded.size != original.size:
        temporary.unlink(missing_ok=True)
        raise SystemExit(f"dimension changed: {original.size} -> {encoded.size}")
    if ImageChops.difference(original.getchannel("A"), encoded.getchannel("A")).getbbox():
        temporary.unlink(missing_ok=True)
        raise SystemExit("alpha channel changed; refusing to replace the atlas")

    dark_psnr = composite_psnr(original, encoded, 24)
    light_psnr = composite_psnr(original, encoded, 240)
    encoded_size = temporary.stat().st_size
    os.replace(temporary, destination)
    print(
        f"{source.name}: {original_size} -> {encoded_size} bytes "
        f"({encoded_size / original_size:.1%}), composite PSNR="
        f"{min(dark_psnr, light_psnr):.2f} dB, alpha exact"
    )


if __name__ == "__main__":
    main()
