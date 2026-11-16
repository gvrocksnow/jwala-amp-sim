"""Builds the macOS / Linux app icons from icons/fire_icon_square_v2.svg (the source of assets/app.ico):

    assets/appicon/jwala_<N>.png   N = 16..1024  Linux hicolor icons; the runtime window icon uses some
    assets/appicon/Jwala.icns                    macOS .app bundle icon

Needs ImageMagick (`magick`) to rasterise the SVG, and Pillow.
Run from anywhere:  python tools/make_app_icons.py
"""
import subprocess
import tempfile
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SVG = ROOT / "icons" / "fire_icon_square_v2.svg"
OUT = ROOT / "assets" / "appicon"
SIZES = [16, 24, 32, 48, 64, 128, 256, 512, 1024]


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        big = Path(tmp) / "icon1024.png"
        # 512-unit viewBox at 288 dpi = 1536 px, then downsampled for clean edges
        subprocess.run(["magick", "-density", "288", "-background", "none", str(SVG), "-resize", "1024x1024", str(big)], check=True)
        src = Image.open(big).convert("RGBA")
        src.load()
    imgs = {}
    for n in SIZES:
        im = src if n == 1024 else src.resize((n, n), Image.LANCZOS)
        im.save(OUT / f"jwala_{n}.png", optimize=True)
        imgs[n] = im
    # Pillow writes every ICNS size from 16 to 1024, taking exact-size images from append_images
    imgs[1024].save(OUT / "Jwala.icns", append_images=[imgs[n] for n in SIZES if n != 1024])
    for p in sorted(OUT.iterdir()):
        print(f"{p.relative_to(ROOT)}  {p.stat().st_size / 1024:.1f} KB")


if __name__ == "__main__":
    main()
