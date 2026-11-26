"""Compose the promo video and framed screenshots from a promo-rec.mjs recording.

usage: python3 promo-compose.py <recording dir> <output dir> <fonts dir>
"""
import json, os, subprocess, sys
from PIL import Image, ImageDraw, ImageFilter, ImageFont

rec, out, fonts = sys.argv[1], sys.argv[2], sys.argv[3]
os.makedirs(out, exist_ok=True)
W, H, FPS = 1920, 1080, 30
F_CAP = ImageFont.truetype(os.path.join(fonts, "seguisb.ttf"), 50)
F_BIG = ImageFont.truetype(os.path.join(fonts, "segoeuib.ttf"), 96)
F_MID = ImageFont.truetype(os.path.join(fonts, "segoeui.ttf"), 44)
F_URL = ImageFont.truetype(os.path.join(fonts, "seguisb.ttf"), 40)

tl = json.load(open(os.path.join(rec, "timeline.json")))
frames = tl["frames"]
times = [f["t"] for f in frames]


def background():
    bg = Image.new("RGB", (W, H), (10, 15, 19))
    glow = Image.new("L", (W, H), 0)
    ImageDraw.Draw(glow).ellipse((W * 0.15, -H * 0.55, W * 0.85, H * 0.45), fill=70)
    glow = glow.filter(ImageFilter.GaussianBlur(180))
    teal = Image.new("RGB", (W, H), (30, 140, 112))
    return Image.composite(teal, bg, glow)


BG = background()
# app window placement: below the caption band, centred
WIN_TOP, WIN_BOTTOM = 150, H - 40
SCALE = (WIN_BOTTOM - WIN_TOP) / 1140
WW, WH = round(1620 * SCALE), round(1140 * SCALE)
WX = (W - WW) // 2
MASK = Image.new("L", (WW, WH), 0)
ImageDraw.Draw(MASK).rounded_rectangle((0, 0, WW - 1, WH - 1), radius=16, fill=255)
SHADOW = Image.new("L", (W, H), 0)
ImageDraw.Draw(SHADOW).rounded_rectangle((WX - 4, WIN_TOP + 14, WX + WW + 4, WIN_TOP + WH + 22), radius=24, fill=150)
SHADOW = SHADOW.filter(ImageFilter.GaussianBlur(28))
BASE = Image.composite(Image.new("RGB", (W, H), (0, 0, 0)), BG, SHADOW)
ImageDraw.Draw(BASE).rounded_rectangle((WX - 1, WIN_TOP - 1, WX + WW, WIN_TOP + WH), radius=17, outline=(40, 54, 64), width=2)

_cache = {}
# Each scene is shown zoomed into the content area (sidebar cropped) so text stays readable on a
# phone. Crop is 1300x915 source px (same aspect as the window); value = top edge per scene.
CROP_X, CROP_W, CROP_H = 296, 1300, 915
CROP_Y = {"overview": 0, "optimize": 200, "standby": 0, "junk": 0, "dupes": 60, "health": 90}
_scene = {"id": "overview"}


def src(name):
    key = (name, _scene["id"])
    if key not in _cache:
        if len(_cache) > 40:
            _cache.clear()
        y = CROP_Y.get(_scene["id"], 0)
        im = Image.open(os.path.join(rec, "frames", name)).convert("RGB").crop((CROP_X, y, CROP_X + CROP_W, y + CROP_H))
        _cache[key] = im.resize((WW, WH), Image.LANCZOS)
    return _cache[key]


def frame_at(t):
    lo, hi = 0, len(times) - 1
    if t <= times[0]:
        return frames[0]["name"]
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if times[mid] <= t:
            lo = mid
        else:
            hi = mid - 1
    return frames[lo]["name"]


def caption_layer(text):
    layer = Image.new("RGBA", (W, 150), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    w = d.textlength(text, font=F_CAP)
    d.text(((W - w) / 2, 46), text, font=F_CAP, fill=(240, 246, 248, 255))
    return layer


def compose(name, cap, cap_alpha=1.0):
    im = BASE.copy()
    im.paste(src(name), (WX, WIN_TOP), MASK)
    if cap is not None and cap_alpha > 0:
        layer = cap.copy()
        if cap_alpha < 1:
            a = layer.split()[3].point(lambda v: int(v * cap_alpha))
            layer.putalpha(a)
        im.paste(layer, (0, 0), layer)
    return im


CAPTIONS = {
    "overview": "MemPilot shows exactly what is in your RAM",
    "optimize": "One click frees memory, with honest before and after numbers",
    "standby": "Standby cache by priority, with an automatic cleaner",
    "junk": "Junk cleaner for temp files, browser and package caches",
    "dupes": "Finds duplicate photos. Extra copies go to the Recycle Bin",
    "health": "Laptop health: battery wear, SSD life, GPU use",
}


def segments(scene):
    """(source start, source end, speed) pieces for a scene."""
    s, e = scene["start"], scene["end"]
    if scene["id"] == "optimize":
        busy_a, busy_b = s + 4.0, e - 3.7  # from the click on Optimize to the result appearing
        return [(s, busy_a, 1.0), (busy_a, busy_b, (busy_b - busy_a) / 1.4), (busy_b, e, 1.0)]
    if scene["id"] == "junk":
        return [(e - 4.6, e, 1.0)]  # start once the scan has finished
    if scene["id"] == "dupes":
        return [(s, e, 1.15)]
    return [(s, e, 1.0)]


clips = []  # list of (list of source times, caption id)
for sc in tl["scenes"]:
    ts = []
    for a, b, speed in segments(sc):
        n = int((b - a) / speed * FPS)
        ts += [a + i * speed / FPS for i in range(n)]
    clips.append((ts, sc["id"]))

XF = int(0.35 * FPS)  # crossfade frames
enc = subprocess.Popen(
    ["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(FPS), "-i", "-",
     "-c:v", "libx264", "-preset", "slow", "-crf", "18", "-pix_fmt", "yuv420p", "-movflags", "+faststart", os.path.join(out, "mempilot-demo.mp4")],
    stdin=subprocess.PIPE,
)
put = lambda im: enc.stdin.write(im.tobytes())
caps = {k: caption_layer(v) for k, v in CAPTIONS.items()}

total = 0
for ci, (ts, cid) in enumerate(clips):
    nxt = clips[ci + 1] if ci + 1 < len(clips) else None
    body = ts if nxt is None else ts[:-XF]
    for i, t in enumerate(body):
        _scene["id"] = cid
        fade_in = min(1.0, i / XF) if ci == 0 else 1.0
        put(compose(frame_at(t), caps[cid], fade_in))
        total += 1
    if nxt:
        for k in range(XF):
            a = (k + 1) / (XF + 1)
            _scene["id"] = cid
            A = compose(frame_at(ts[len(ts) - XF + k]), caps[cid], 1 - a)
            _scene["id"] = nxt[1]
            B = compose(frame_at(nxt[0][k]), caps[nxt[1]], a)
            put(Image.blend(A, B, a))
            total += 1
        clips[ci + 1] = (nxt[0][XF:], nxt[1])
        # the next clip's first XF frames were used in the blend


def end_card():
    im = BG.copy()
    d = ImageDraw.Draw(im)
    icon = Image.open(os.path.join(os.path.dirname(__file__), "..", "app-icon.png")).convert("RGBA").resize((150, 150), Image.LANCZOS)
    im.paste(icon, ((W - 150) // 2, 250), icon)
    for text, font, y, col in [("MemPilot", F_BIG, 430, (240, 246, 248)), ("Free and open source RAM and laptop care for Windows", F_MID, 570, (160, 178, 188)),
                               ("github.com/romilp619/mempilot", F_URL, 680, (46, 204, 160))]:
        w = d.textlength(text, font=font)
        d.text(((W - w) / 2, y), text, font=font, fill=col)
    return im


_scene["id"] = clips[-1][1]
last = compose(frame_at(clips[-1][0][-1]), caps[clips[-1][1]])
card = end_card()
for k in range(int(0.5 * FPS)):
    put(Image.blend(last, card, (k + 1) / (0.5 * FPS)))
for _ in range(int(3.2 * FPS)):
    put(card)
enc.stdin.close()
enc.wait()
print("video frames", total, "->", os.path.join(out, "mempilot-demo.mp4"))

# ---- framed screenshots (16:9, 1600x900 is what X shows uncropped)
SW, SH = 1600, 900
for f in sorted(os.listdir(os.path.join(rec, "shots"))):
    shot = Image.open(os.path.join(rec, "shots", f)).convert("RGB")
    bg = background().resize((SW, SH), Image.LANCZOS)
    s = (SH - 70) / shot.height
    sw, sh = round(shot.width * s), round(shot.height * s)
    shot = shot.resize((sw, sh), Image.LANCZOS)
    x, y = (SW - sw) // 2, (SH - sh) // 2
    m = Image.new("L", (sw, sh), 0)
    ImageDraw.Draw(m).rounded_rectangle((0, 0, sw - 1, sh - 1), radius=14, fill=255)
    sd = Image.new("L", (SW, SH), 0)
    ImageDraw.Draw(sd).rounded_rectangle((x - 2, y + 10, x + sw + 2, y + sh + 16), radius=20, fill=140)
    bg = Image.composite(Image.new("RGB", (SW, SH)), bg, sd.filter(ImageFilter.GaussianBlur(22)))
    bg.paste(shot, (x, y), m)
    bg.save(os.path.join(out, f), optimize=True)
print("screenshots", len(os.listdir(os.path.join(rec, "shots"))))
