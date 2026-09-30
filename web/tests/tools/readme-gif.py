#!/usr/bin/env python3
"""Turn the walkthrough frames (tools/readme-media.mjs) into docs/assets/demo.gif.

Frames keep their real timing (repaint-to-repaint), identical frames merge, text stays crisp
(no dithering), and the result is sized for a README (960 px wide).
    python3 web/tests/tools/readme-gif.py
"""
import json
import os
import sys

from PIL import Image, ImageChops

HERE = os.path.dirname(os.path.abspath(__file__))
ASSETS = os.path.normpath(os.path.join(HERE, "..", "..", "..", "docs", "assets"))
FRAMES = os.path.join(ASSETS, ".frames")
WIDTH = int(os.environ.get("GIF_WIDTH", "960"))

index = json.load(open(os.path.join(FRAMES, "index.json")))
# Timestamps of everything (frames and holds) give each frame its on-screen time.
stamps = [e["t"] for e in index]
frames, durations = [], []
for i, e in enumerate(index):
    if "file" not in e:
        continue
    nxt = next((x["t"] for x in index[i + 1:] if "file" in x), None)
    ms = int(((nxt if nxt is not None else stamps[-1] + 2.5) - e["t"]) * 1000)
    img = Image.open(os.path.join(FRAMES, e["file"])).convert("RGB")
    if img.width != WIDTH:
        img = img.resize((WIDTH, round(img.height * WIDTH / img.width)), Image.LANCZOS)
    if frames and ImageChops.difference(img, frames[-1]).getbbox() is None:
        durations[-1] += ms
        continue
    frames.append(img)
    durations.append(ms)

durations = [max(60, min(d, 4000)) for d in durations]
durations[-1] = max(durations[-1], 3000)  # rest on the last screen before the loop
palette = [f.quantize(colors=255, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE) for f in frames]
out = os.path.join(ASSETS, "demo.gif")
palette[0].save(out, save_all=True, append_images=palette[1:], duration=durations, loop=0, optimize=True, disposal=1)
print(f"demo.gif: {len(frames)} frames, {sum(durations) / 1000:.1f} s, {os.path.getsize(out) / 1e6:.1f} MB")
