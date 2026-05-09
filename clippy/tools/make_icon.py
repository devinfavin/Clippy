"""Generate the source app icon for Clippy.

Outputs `tools/icon.png` at 1024x1024. Pipe through `pnpm tauri icon` to
generate the ICO/ICNS/PNG set that the bundler consumes.

Design: rounded-square gradient tile (blue accent) with a play triangle cut
out, on a near-black background — same visual language as the in-app brand
mark, but bigger and more confident.
"""
from PIL import Image, ImageDraw, ImageFilter
import os

SIZE = 1024
OUT = os.path.join(os.path.dirname(__file__), "icon.png")


def lerp_color(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))


def main():
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))

    # ----- background tile (rounded square) with diagonal gradient -----
    pad = int(SIZE * 0.08)
    radius = int(SIZE * 0.20)
    grad_top = (0x4F, 0x9D, 0xFF)   # accent
    grad_bot = (0x1E, 0x52, 0x9C)   # darker accent
    bg = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    bg_draw = ImageDraw.Draw(bg)

    # Build gradient as a vertical strip, then apply rounded mask.
    grad = Image.new("RGBA", (1, SIZE), (0, 0, 0, 0))
    for y in range(SIZE):
        t = y / (SIZE - 1)
        c = lerp_color(grad_top, grad_bot, t)
        grad.putpixel((0, y), (*c, 255))
    grad = grad.resize((SIZE - pad * 2, SIZE - pad * 2))

    mask = Image.new("L", (SIZE - pad * 2, SIZE - pad * 2), 0)
    md = ImageDraw.Draw(mask)
    md.rounded_rectangle(
        (0, 0, SIZE - pad * 2 - 1, SIZE - pad * 2 - 1),
        radius=radius,
        fill=255,
    )

    bg.paste(grad, (pad, pad), mask)

    # Soft drop shadow under the tile
    shadow = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    sd = ImageDraw.Draw(shadow)
    sd.rounded_rectangle(
        (pad, pad + int(SIZE * 0.02), SIZE - pad - 1, SIZE - pad - 1 + int(SIZE * 0.02)),
        radius=radius,
        fill=(0, 0, 0, 180),
    )
    shadow = shadow.filter(ImageFilter.GaussianBlur(radius=int(SIZE * 0.025)))
    img.alpha_composite(shadow)
    img.alpha_composite(bg)

    # ----- play triangle (cut-out style: dark on bright tile) -----
    cx, cy = SIZE // 2, SIZE // 2
    tri_w = int(SIZE * 0.32)
    tri_h = int(SIZE * 0.36)
    # Slightly biased right so the triangle sits balanced visually.
    bias_x = int(SIZE * 0.025)
    pts = [
        (cx - tri_w // 2 + bias_x, cy - tri_h // 2),
        (cx - tri_w // 2 + bias_x, cy + tri_h // 2),
        (cx + tri_w // 2 + bias_x, cy),
    ]
    tri_layer = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    td = ImageDraw.Draw(tri_layer)
    td.polygon(pts, fill=(14, 18, 22, 255))  # matches --bg
    img.alpha_composite(tri_layer)

    # ----- two thin "cut" lines below (clipping marks) -----
    line_y = int(SIZE * 0.78)
    line_w = int(SIZE * 0.42)
    line_x = (SIZE - line_w) // 2
    line_thick = max(2, int(SIZE * 0.012))

    line_layer = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    ld = ImageDraw.Draw(line_layer)
    ld.rounded_rectangle(
        (line_x, line_y, line_x + line_w, line_y + line_thick),
        radius=line_thick // 2,
        fill=(255, 255, 255, 220),
    )
    # Two cut tick marks on the line
    tick_h = int(SIZE * 0.045)
    for fx in (0.32, 0.68):
        tx = line_x + int(line_w * fx)
        ld.rounded_rectangle(
            (tx - line_thick, line_y - tick_h // 2, tx + line_thick, line_y + line_thick + tick_h // 2),
            radius=line_thick,
            fill=(255, 255, 255, 240),
        )
    img.alpha_composite(line_layer)

    img.save(OUT, format="PNG")
    print(f"wrote {OUT} ({SIZE}x{SIZE})")


if __name__ == "__main__":
    main()
