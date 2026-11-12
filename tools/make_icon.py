#!/usr/bin/env python3
"""Draws the SimQ app icon and writes assets/icon.png, assets/icon-256.png
and (on macOS) assets/SimQ.icns.

The icon is a queue: three rows with a status dot each, in the same colours
the app uses for in progress, queued and done.

Requires Pillow:  python3 tools/make_icon.py
"""
import pathlib
import shutil
import subprocess
import tempfile

from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
SCALE = 4  # supersampling factor
SIZE = 1024


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def draw_icon():
    s = SIZE * SCALE
    # Background: vertical gradient clipped to a rounded square.
    top, bottom = (58, 124, 255), (22, 58, 176)
    gradient = Image.new("RGB", (1, s))
    for y in range(s):
        gradient.putpixel((0, y), lerp(top, bottom, y / (s - 1)))
    gradient = gradient.resize((s, s))

    inset, radius = 100 * SCALE, 190 * SCALE
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (inset, inset, s - inset, s - inset), radius=radius, fill=255
    )
    icon = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    icon.paste(gradient, (0, 0), mask)

    # A soft highlight along the top edge.
    glow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(glow).ellipse(
        (-s // 4, -s // 2, s + s // 4, s // 2), fill=(255, 255, 255, 34)
    )
    icon = Image.alpha_composite(icon, Image.composite(glow, Image.new("RGBA", (s, s)), mask))

    draw = ImageDraw.Draw(icon)
    rows = [
        # (y centre, dot colour, bar length, bar alpha)
        (372, (255, 186, 82), 350, 255),   # in progress
        (512, (190, 216, 255), 280, 215),  # queued
        (652, (126, 226, 142), 205, 170),  # done
    ]
    dot_x, dot_r = 302, 46
    bar_x, bar_h = 392, 72
    for y, dot, length, alpha in rows:
        draw.ellipse(
            ((dot_x - dot_r) * SCALE, (y - dot_r) * SCALE,
             (dot_x + dot_r) * SCALE, (y + dot_r) * SCALE),
            fill=dot + (255,),
        )
        bar = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        ImageDraw.Draw(bar).rounded_rectangle(
            (bar_x * SCALE, (y - bar_h // 2) * SCALE,
             (bar_x + length) * SCALE, (y + bar_h // 2) * SCALE),
            radius=bar_h // 2 * SCALE,
            fill=(255, 255, 255, alpha),
        )
        icon = Image.alpha_composite(icon, bar)
        draw = ImageDraw.Draw(icon)
    return icon.resize((SIZE, SIZE), Image.LANCZOS)


def main():
    ASSETS.mkdir(exist_ok=True)
    icon = draw_icon()
    icon.save(ASSETS / "icon.png")
    icon.resize((256, 256), Image.LANCZOS).save(ASSETS / "icon-256.png")

    if shutil.which("iconutil"):
        with tempfile.TemporaryDirectory() as tmp:
            iconset = pathlib.Path(tmp) / "SimQ.iconset"
            iconset.mkdir()
            for size in (16, 32, 128, 256, 512):
                icon.resize((size, size), Image.LANCZOS).save(iconset / f"icon_{size}x{size}.png")
                icon.resize((size * 2, size * 2), Image.LANCZOS).save(
                    iconset / f"icon_{size}x{size}@2x.png"
                )
            subprocess.run(
                ["iconutil", "-c", "icns", str(iconset), "-o", str(ASSETS / "SimQ.icns")],
                check=True,
            )
    print("wrote", ", ".join(sorted(p.name for p in ASSETS.iterdir())))


if __name__ == "__main__":
    main()
