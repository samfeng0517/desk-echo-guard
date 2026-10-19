"""Generate echoguard/assets/icon.ico + icon.png (needs Pillow).

    python tools/make_icon.py

Design: rounded square, white shield, inside it a sound-level glyph whose
right half (the "echo") is cut off by the shield edge.
"""

import os

from PIL import Image, ImageDraw

S = 1024  # draw large, downsample for crisp small sizes
OUT = os.path.join(os.path.dirname(__file__), "..", "echoguard", "assets")


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))


def draw(size=S):
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    # vertical gradient background in a rounded square
    top, bottom = (37, 99, 235), (13, 42, 120)
    grad = Image.new("RGBA", (size, size))
    gd = ImageDraw.Draw(grad)
    for y in range(size):
        gd.line([(0, y), (size, y)], fill=lerp(top, bottom, y / size) + (255,))
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, size - 1, size - 1], radius=int(size * 0.22),
                                           fill=255)
    img.paste(grad, (0, 0), mask)
    d = ImageDraw.Draw(img)

    # shield
    cx = size / 2
    w, top_y, mid_y, bot_y = size * 0.62, size * 0.15, size * 0.55, size * 0.88
    shield = [(cx - w / 2, top_y + size * 0.06), (cx, top_y), (cx + w / 2, top_y + size * 0.06),
              (cx + w / 2, mid_y), (cx, bot_y), (cx - w / 2, mid_y)]
    # smooth the lower part with extra points along a curve
    pts = shield[:4]
    for i in range(1, 12):
        t = i / 12
        pts.append((cx + w / 2 * (1 - t ** 1.8), mid_y + (bot_y - mid_y) * t))
    pts.append((cx, bot_y))
    for i in range(1, 12):
        t = 1 - i / 12
        pts.append((cx - w / 2 * (1 - t ** 1.8), mid_y + (bot_y - mid_y) * t))
    d.polygon(pts, fill=(255, 255, 255, 255))

    # sound bars inside the shield (center tall, sides shorter)
    bar_w = size * 0.07
    gap = size * 0.045
    heights = [0.16, 0.30, 0.42, 0.30, 0.16]
    color = (29, 78, 216, 255)
    x0 = cx - (len(heights) * bar_w + (len(heights) - 1) * gap) / 2
    cy = size * 0.47
    for i, h in enumerate(heights):
        x = x0 + i * (bar_w + gap)
        hh = h * size
        fill = color if i <= 2 else (148, 163, 184, 255)  # the "echo" side is muted
        d.rounded_rectangle([x, cy - hh / 2, x + bar_w, cy + hh / 2], radius=bar_w / 2, fill=fill)
    # slash over the muted echo bars
    d.line([(cx + gap * 0.6, cy + size * 0.17), (cx + w / 2 - size * 0.05, cy - size * 0.17)],
           fill=(220, 38, 38, 255), width=int(size * 0.045))
    return img


def main():
    os.makedirs(OUT, exist_ok=True)
    big = draw()
    big.resize((256, 256), Image.LANCZOS).save(os.path.join(OUT, "icon.png"))
    sizes = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    big.save(os.path.join(OUT, "icon.ico"), sizes=sizes)
    print("wrote", os.path.abspath(OUT))


if __name__ == "__main__":
    main()
