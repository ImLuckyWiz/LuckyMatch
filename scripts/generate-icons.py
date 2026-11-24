"""Generate LuckyMatch app icons (Windows .ico + Android adaptive icon) from one geometry.

The mark: a four-leaf clover where three leaves are hearts and the fourth "leaf"
is a set of sound-wave arcs -- a lucky clover that listens.

Usage:  python scripts/generate-icons.py [--preview]
Requires: pip install resvg-py pillow
"""

from __future__ import annotations

import io
import math
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BRAND = ROOT / "assets" / "branding"
ANDROID_RES = ROOT / "android" / "app" / "src" / "main" / "res"

# ---------------------------------------------------------------- palette
BG_TOP = "#111A33"
BG_BOTTOM = "#1B1442"
LEAF_A = "#5EEAD4"  # mint / teal
LEAF_B = "#38BDF8"  # sky (app accent)
LEAF_C = "#818CF8"  # indigo
WAVE = "#F0F9FF"

# ---------------------------------------------------------------- geometry (108-unit canvas, Android adaptive size)
C = 54.0          # centre
A = 16.0          # heart "square" side
GAP = 4.0         # distance from centre to each heart tip
LEAF_ANGLES = [225, 315, 135]   # up-left, up-right, down-left (deg, y-down)
WAVE_ANGLE = 45                 # down-right
WAVE_RADII = [11.5, 19.5, 27.5]
WAVE_SPAN = [30, 34, 36]        # half-angle of each arc, degrees
WAVE_STROKE = 4.6


def f(v: float) -> str:
    s = f"{v:.3f}".rstrip("0").rstrip(".")
    return s if s != "-0" else "0"


def heart_path(angle_deg: float) -> str:
    """Heart = square rotated 45deg with two semicircles on its outer sides.

    Tip points at the centre; the heart extends outward along `angle_deg`.
    """
    th = math.radians(angle_deg)
    ux, uy = math.cos(th), math.sin(th)        # outward axis
    vx, vy = -uy, ux                           # lateral axis
    h = A / math.sqrt(2)

    def pt(along: float, lat: float) -> tuple[float, float]:
        return (C + ux * along + vx * lat, C + uy * along + vy * lat)

    tip = pt(GAP, 0)
    left = pt(GAP + h, -h)
    top = pt(GAP + 2 * h, 0)
    right = pt(GAP + h, h)
    r = A / 2
    return (
        f"M{f(tip[0])},{f(tip[1])} L{f(left[0])},{f(left[1])} "
        f"A{f(r)},{f(r)} 0 0,1 {f(top[0])},{f(top[1])} "
        f"A{f(r)},{f(r)} 0 0,1 {f(right[0])},{f(right[1])} Z"
    )


def arc_path(radius: float, half_span: float) -> str:
    a0 = math.radians(WAVE_ANGLE - half_span)
    a1 = math.radians(WAVE_ANGLE + half_span)
    x0, y0 = C + radius * math.cos(a0), C + radius * math.sin(a0)
    x1, y1 = C + radius * math.cos(a1), C + radius * math.sin(a1)
    return f"M{f(x0)},{f(y0)} A{f(radius)},{f(radius)} 0 0,1 {f(x1)},{f(y1)}"


LEAVES = [heart_path(a) for a in LEAF_ANGLES]
WAVES = [arc_path(r, s) for r, s in zip(WAVE_RADII, WAVE_SPAN)]
DOT_R = 2.4  # small "source" dot at the centre of the waves


# ---------------------------------------------------------------- SVG output
def mark_svg(scale: float = 1.0, offset: float = 0.0) -> str:
    """The clover mark, in a <g> transformed from the 108 canvas."""
    leaves = "\n".join(f'    <path d="{d}" fill="url(#leaf)"/>' for d in LEAVES)
    waves = "\n".join(
        f'    <path d="{d}" fill="none" stroke="{WAVE}" stroke-width="{WAVE_STROKE}" '
        f'stroke-linecap="round"/>'
        for d in WAVES
    )
    return f"""  <g transform="translate({f(offset)} {f(offset)}) scale({f(scale)})">
{leaves}
{waves}
    <circle cx="{f(C)}" cy="{f(C)}" r="{DOT_R}" fill="{WAVE}"/>
  </g>"""


DEFS = f"""  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="{BG_TOP}"/>
      <stop offset="1" stop-color="{BG_BOTTOM}"/>
    </linearGradient>
    <radialGradient id="glow" cx="0.5" cy="0.5" r="0.5">
      <stop offset="0" stop-color="{LEAF_B}" stop-opacity="0.28"/>
      <stop offset="1" stop-color="{LEAF_B}" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="leaf" gradientUnits="userSpaceOnUse" x1="22" y1="22" x2="80" y2="80">
      <stop offset="0" stop-color="{LEAF_A}"/>
      <stop offset="0.5" stop-color="{LEAF_B}"/>
      <stop offset="1" stop-color="{LEAF_C}"/>
    </linearGradient>
  </defs>"""


def full_icon_svg(size: int = 1024, rounded: bool = True) -> str:
    """Full-bleed app icon: rounded-square background + glow + mark."""
    r = size * 0.225 if rounded else 0
    # mark occupies the 108 canvas scaled so that 108 -> size*1.32 (adaptive-like crop)
    scale = size * 1.30 / 108
    offset = (size - 108 * scale) / 2
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">
{DEFS}
  <rect width="{size}" height="{size}" rx="{f(r)}" fill="url(#bg)"/>
  <circle cx="{size/2}" cy="{size/2}" r="{size*0.46}" fill="url(#glow)"/>
{mark_svg(scale, offset)}
</svg>
"""


def transparent_mark_svg(size: int = 1024) -> str:
    scale = size / 108 * 1.45
    offset = (size - 108 * scale) / 2
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">
{DEFS}
{mark_svg(scale, offset)}
</svg>
"""


# ---------------------------------------------------------------- Android vector drawables
def android_foreground() -> str:
    leaf_grad = f"""        <aapt:attr name="android:fillColor">
            <gradient
                android:type="linear"
                android:startX="22" android:startY="22"
                android:endX="80" android:endY="80">
                <item android:offset="0" android:color="#FF{LEAF_A[1:]}" />
                <item android:offset="0.5" android:color="#FF{LEAF_B[1:]}" />
                <item android:offset="1" android:color="#FF{LEAF_C[1:]}" />
            </gradient>
        </aapt:attr>"""
    leaves = "\n".join(
        f"""    <path android:pathData="{d}">
{leaf_grad}
    </path>"""
        for d in LEAVES
    )
    waves = "\n".join(
        f"""    <path
        android:pathData="{d}"
        android:strokeColor="#FF{WAVE[1:]}"
        android:strokeWidth="{WAVE_STROKE}"
        android:strokeLineCap="round" />"""
        for d in WAVES
    )
    dot = (
        f"M{f(C - DOT_R)},{f(C)} a{DOT_R},{DOT_R} 0 1,0 {f(2 * DOT_R)},0 "
        f"a{DOT_R},{DOT_R} 0 1,0 {f(-2 * DOT_R)},0z"
    )
    return f"""<?xml version="1.0" encoding="utf-8"?>
<!-- Generated by scripts/generate-icons.py - do not edit by hand. -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    xmlns:aapt="http://schemas.android.com/aapt"
    android:width="108dp"
    android:height="108dp"
    android:viewportWidth="108"
    android:viewportHeight="108">
{leaves}
{waves}
    <path
        android:fillColor="#FF{WAVE[1:]}"
        android:pathData="{dot}" />
</vector>
"""


def android_background() -> str:
    return f"""<?xml version="1.0" encoding="utf-8"?>
<!-- Generated by scripts/generate-icons.py - do not edit by hand. -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    xmlns:aapt="http://schemas.android.com/aapt"
    android:width="108dp"
    android:height="108dp"
    android:viewportWidth="108"
    android:viewportHeight="108">
    <path android:pathData="M0,0h108v108h-108z">
        <aapt:attr name="android:fillColor">
            <gradient
                android:type="linear"
                android:startX="0" android:startY="0"
                android:endX="108" android:endY="108">
                <item android:offset="0" android:color="#FF{BG_TOP[1:]}" />
                <item android:offset="1" android:color="#FF{BG_BOTTOM[1:]}" />
            </gradient>
        </aapt:attr>
    </path>
    <path android:pathData="M54,4.32a49.68,49.68 0 1,0 0.01,0z">
        <aapt:attr name="android:fillColor">
            <gradient
                android:type="radial"
                android:centerX="54" android:centerY="54"
                android:gradientRadius="49.68">
                <item android:offset="0" android:color="#47{LEAF_B[1:]}" />
                <item android:offset="1" android:color="#00{LEAF_B[1:]}" />
            </gradient>
        </aapt:attr>
    </path>
</vector>
"""


ANDROID_MONO = """<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@drawable/ic_launcher_background" />
    <foreground android:drawable="@drawable/ic_launcher_foreground" />
    <monochrome android:drawable="@drawable/ic_launcher_monochrome" />
</adaptive-icon>
"""


def android_monochrome() -> str:
    leaves = "\n".join(
        f'    <path android:fillColor="#FFFFFFFF" android:pathData="{d}" />' for d in LEAVES
    )
    waves = "\n".join(
        f"""    <path android:pathData="{d}" android:strokeColor="#FFFFFFFF"
        android:strokeWidth="{WAVE_STROKE}" android:strokeLineCap="round" />"""
        for d in WAVES
    )
    dot = (
        f"M{f(C - DOT_R)},{f(C)} a{DOT_R},{DOT_R} 0 1,0 {f(2 * DOT_R)},0 "
        f"a{DOT_R},{DOT_R} 0 1,0 {f(-2 * DOT_R)},0z"
    )
    return f"""<?xml version="1.0" encoding="utf-8"?>
<!-- Generated by scripts/generate-icons.py - themed (Material You) icon. -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="108dp"
    android:height="108dp"
    android:viewportWidth="108"
    android:viewportHeight="108">
{leaves}
{waves}
    <path android:fillColor="#FFFFFFFF" android:pathData="{dot}" />
</vector>
"""


# ---------------------------------------------------------------- raster helpers
def render(svg: str, size: int):
    import resvg_py
    from PIL import Image

    data = resvg_py.svg_to_bytes(svg_string=svg, width=size, height=size)
    return Image.open(io.BytesIO(bytes(data))).convert("RGBA")


def main() -> None:
    BRAND.mkdir(parents=True, exist_ok=True)
    (BRAND / "luckymatch-icon.svg").write_text(full_icon_svg(1024), encoding="utf-8")
    (BRAND / "luckymatch-mark.svg").write_text(transparent_mark_svg(1024), encoding="utf-8")

    master = render(full_icon_svg(1024), 1024)
    master.save(BRAND / "luckymatch-icon-1024.png")
    render(transparent_mark_svg(1024), 1024).save(BRAND / "luckymatch-mark-1024.png")

    if "--preview" in sys.argv:
        return

    # Windows .ico: render each size natively for crisp small icons.
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    frames = [render(full_icon_svg(s), s) for s in sizes]
    frames[-1].save(
        BRAND / "luckymatch.ico",
        format="ICO",
        sizes=[(s, s) for s in sizes],
        append_images=frames[:-1],
    )

    # Android
    (ANDROID_RES / "drawable" / "ic_launcher_foreground.xml").write_text(android_foreground(), encoding="utf-8")
    (ANDROID_RES / "drawable" / "ic_launcher_background.xml").write_text(android_background(), encoding="utf-8")
    (ANDROID_RES / "drawable" / "ic_launcher_monochrome.xml").write_text(android_monochrome(), encoding="utf-8")
    for name in ("ic_launcher.xml", "ic_launcher_round.xml"):
        (ANDROID_RES / "mipmap-anydpi-v26" / name).write_text(ANDROID_MONO, encoding="utf-8")
    # Play Store listing icon (512x512, full-bleed square; Play applies its own mask)
    render(full_icon_svg(512, rounded=False), 512).save(BRAND / "play-store-icon-512.png")
    print("Icons written to", BRAND, "and", ANDROID_RES)


if __name__ == "__main__":
    main()
