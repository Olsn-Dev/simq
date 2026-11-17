#!/usr/bin/env python3
"""Draws the small icons used on buttons and writes them to assets/icons/.

Every icon is white on a transparent background, so the app can tint it with
the text colour of the current theme.

Requires Pillow:  python3 tools/make_icons.py
"""
import math
import pathlib

from PIL import Image, ImageChops, ImageDraw

OUT = pathlib.Path(__file__).resolve().parent.parent / "assets" / "icons"
SIZE = 64  # final size in pixels
SCALE = 8  # supersampling factor
S = SIZE * SCALE
WHITE = 255


def canvas():
    image = Image.new("L", (S, S), 0)
    return image, ImageDraw.Draw(image)


def p(*values):
    """Icon coordinates (0-64) to canvas pixels."""
    return tuple(v * SCALE for v in values)


def line(draw, points, width=6):
    """A line with round ends and joints."""
    pts = [p(*pt) for pt in points]
    draw.line(pts, fill=WHITE, width=width * SCALE, joint="curve")
    r = width * SCALE / 2
    for x, y in pts:
        draw.ellipse((x - r, y - r, x + r, y + r), fill=WHITE)


def ring(draw, cx, cy, radius, width=6):
    draw.ellipse(p(cx - radius, cy - radius, cx + radius, cy + radius), outline=WHITE, width=width * SCALE)


def dot(draw, cx, cy, radius):
    draw.ellipse(p(cx - radius, cy - radius, cx + radius, cy + radius), fill=WHITE)


def plus():
    image, draw = canvas()
    line(draw, [(32, 14), (32, 50)], 7)
    line(draw, [(14, 32), (50, 32)], 7)
    return image


def close():
    image, draw = canvas()
    line(draw, [(17, 17), (47, 47)], 6)
    line(draw, [(47, 17), (17, 47)], 6)
    return image


def more():
    image, draw = canvas()
    for x in (14, 32, 50):
        dot(draw, x, 32, 5.5)
    return image


def back():
    image, draw = canvas()
    line(draw, [(39, 14), (21, 32), (39, 50)], 7)
    return image


def search():
    image, draw = canvas()
    ring(draw, 27, 27, 16, 6)
    line(draw, [(40, 40), (53, 53)], 7)
    return image


def download():
    image, draw = canvas()
    line(draw, [(32, 10), (32, 38)], 6)
    line(draw, [(20, 27), (32, 39), (44, 27)], 6)
    line(draw, [(12, 44), (12, 53), (52, 53), (52, 44)], 6)
    return image


def export():
    image, draw = canvas()
    line(draw, [(32, 40), (32, 11)], 6)
    line(draw, [(20, 22), (32, 10), (44, 22)], 6)
    line(draw, [(12, 40), (12, 53), (52, 53), (52, 40)], 6)
    return image


def sun():
    image, draw = canvas()
    dot(draw, 32, 32, 11)
    for i in range(8):
        a = math.tau * i / 8
        line(draw, [(32 + 18 * math.cos(a), 32 + 18 * math.sin(a)),
                    (32 + 25 * math.cos(a), 32 + 25 * math.sin(a))], 5)
    return image


def moon():
    image, draw = canvas()
    dot(draw, 30, 33, 22)
    cut, cut_draw = canvas()
    dot(cut_draw, 42, 24, 19)
    return ImageChops.subtract(image, cut)


def gear():
    image, draw = canvas()
    for i in range(8):
        a = math.tau * i / 8
        line(draw, [(32 + 16 * math.cos(a), 32 + 16 * math.sin(a)),
                    (32 + 24 * math.cos(a), 32 + 24 * math.sin(a))], 9)
    dot(draw, 32, 32, 18)
    hole, hole_draw = canvas()
    dot(hole_draw, 32, 32, 8)
    return ImageChops.subtract(image, hole)


def sync():
    image, draw = canvas()
    box = p(12, 12, 52, 52)
    draw.arc(box, start=200, end=340, fill=WHITE, width=6 * SCALE)
    draw.arc(box, start=20, end=160, fill=WHITE, width=6 * SCALE)
    # Arrow heads at the end of each arc.
    for angle, direction in ((340, 1), (160, 1)):
        a = math.radians(angle)
        x, y = 32 + 20 * math.cos(a), 32 + 20 * math.sin(a)
        t = a + direction * math.pi / 2  # along the arc
        tip = (x + 9 * math.cos(t), y + 9 * math.sin(t))
        left = (x + 9 * math.cos(a), y + 9 * math.sin(a))
        right = (x - 9 * math.cos(a), y - 9 * math.sin(a))
        draw.polygon([p(*tip), p(*left), p(*right)], fill=WHITE)
    return image


def comment():
    image, draw = canvas()
    draw.rounded_rectangle(p(9, 11, 55, 44), radius=9 * SCALE, outline=WHITE, width=6 * SCALE)
    draw.polygon([p(20, 42), p(20, 56), p(34, 42)], fill=WHITE)
    return image


def copy():
    image, draw = canvas()
    draw.rounded_rectangle(p(22, 22, 54, 54), radius=6 * SCALE, outline=WHITE, width=6 * SCALE)
    line(draw, [(42, 13), (16, 13), (13, 16), (13, 42)], 6)
    return image


ICONS = {
    "plus": plus, "close": close, "more": more, "back": back, "search": search,
    "download": download, "export": export, "sun": sun, "moon": moon,
    "gear": gear, "sync": sync, "comment": comment, "copy": copy,
}


def main():
    """Draws all icons, or only those named on the command line."""
    import sys
    wanted = sys.argv[1:] or list(ICONS)
    OUT.mkdir(parents=True, exist_ok=True)
    for name in wanted:
        draw_icon = ICONS[name]
        alpha = draw_icon().resize((SIZE, SIZE), Image.LANCZOS)
        icon = Image.new("RGBA", (SIZE, SIZE), (255, 255, 255, 0))
        icon.putalpha(alpha)
        icon.save(OUT / f"{name}.png", optimize=True)
    print("wrote", ", ".join(sorted(wanted)))


if __name__ == "__main__":
    main()
