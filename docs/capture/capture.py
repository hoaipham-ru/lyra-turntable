"""Render README screenshots of the Lyra UI with a mocked Beefweb API.

Usage (from the repo root, with `npm run dev` running on :1420):
    python docs/capture/capture.py
Writes PNGs + a webm clip to docs/images/, then turns the clip into spin.gif with ffmpeg.
"""
import json, time, pathlib, subprocess
from urllib.parse import urlparse, unquote
from playwright.sync_api import sync_playwright

OUT = pathlib.Path(__file__).resolve().parents[1] / "images"
OUT.mkdir(parents=True, exist_ok=True)
URL = "http://localhost:1420/"

PLAYLISTS = [
    {"id": "p1", "index": 0, "title": "Default Playlist", "isCurrent": False, "itemCount": 0, "totalTime": 0},
    {"id": "p2", "index": 1, "title": "Hi-Res Jazz · 24/96", "isCurrent": True, "itemCount": 8, "totalTime": 2900},
    {"id": "p3", "index": 2, "title": "DSD128", "isCurrent": False, "itemCount": 3, "totalTime": 1500},
]
# artist, title, album, seconds, filename
TRACKS = [
    ["Miles Davis", "So What", "Kind of Blue", "562", "01 So What.flac"],
    ["Miles Davis", "Blue in Green", "Kind of Blue", "337", "03 Blue in Green.flac"],
    ["Bill Evans Trio", "Waltz for Debby", "Waltz for Debby", "419", "02 Waltz for Debby.flac"],
    ["John Coltrane", "Naima", "Giant Steps", "261", "06 Naima.flac"],
    ["Dave Brubeck Quartet", "Take Five", "Time Out", "324", "03 Take Five.flac"],
    ["Chet Baker", "Almost Blue", "Chet Baker Sings Again", "283", "01 Almost Blue.dsf"],
    ["Norah Jones", "Don't Know Why", "Come Away with Me", "186", "02 Don't Know Why.flac"],
    ["d4vd", "Here With Me", "Petals to Thorns", "242", "Here With Me.flac"],
]
PLAYING = 1
T0 = time.time() - 74  # 1:14 into the track

ART = """<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 300 300'>
<defs><radialGradient id='g' cx='.35' cy='.3' r='.9'><stop offset='0' stop-color='#3f7f8c'/><stop offset='1' stop-color='#0f2a33'/></radialGradient></defs>
<rect width='300' height='300' fill='url(#g)'/>
<circle cx='190' cy='120' r='70' fill='#d9a441' opacity='.85'/>
<circle cx='120' cy='190' r='88' fill='none' stroke='#e9dcc4' stroke-width='3' opacity='.6'/>
<rect x='0' y='228' width='300' height='72' fill='#0b1d24' opacity='.7'/>
<text x='24' y='270' font-family='Georgia,serif' font-style='italic' font-size='30' fill='#e9dcc4'>blue in green</text>
</svg>"""

CORS = {"Access-Control-Allow-Origin": "*", "Access-Control-Allow-Headers": "*", "Access-Control-Allow-Methods": "*"}


def handle(route):
    req = route.request
    path = unquote(urlparse(req.url).path)
    if req.method in ("OPTIONS", "POST"):
        return route.fulfill(status=204, headers=CORS)
    if path == "/api/artwork/current":
        return route.fulfill(status=200, headers={**CORS, "Content-Type": "image/svg+xml"}, body=ART)
    if path == "/api/player":
        t = TRACKS[PLAYING]
        body = {"player": {
            "info": {"name": "foobar2000", "version": "2.25.5", "pluginVersion": "0.10"},
            "playbackState": "playing",
            "activeItem": {"playlistId": "p2", "playlistIndex": 1, "index": PLAYING,
                           "position": (time.time() - T0) % int(t[3]), "duration": float(t[3]), "columns": t},
            "volume": {"type": "db", "min": -100.0, "max": 0.0, "value": 0.0, "isMuted": False},
        }}
    elif path == "/api/playlists":
        body = {"playlists": PLAYLISTS}
    elif path.startswith("/api/playlists/p2/items"):
        body = {"playlistItems": {"offset": 0, "totalCount": len(TRACKS), "items": [{"columns": c} for c in TRACKS]}}
    elif path.startswith("/api/playlists/"):
        body = {"playlistItems": {"offset": 0, "totalCount": 0, "items": []}}
    else:
        body = {}
    route.fulfill(status=200, headers={**CORS, "Content-Type": "application/json"}, body=json.dumps(body))


def page_for(browser, video=False):
    opts = dict(viewport={"width": 1040, "height": 560}, device_scale_factor=2)
    if video:
        opts = dict(viewport={"width": 1040, "height": 560}, record_video_dir=str(OUT / "_video"),
                    record_video_size={"width": 1040, "height": 560})
    ctx = browser.new_context(**opts)
    ctx.route("http://localhost:8880/**", handle)
    page = ctx.new_page()
    if video:
        page.add_init_script("document.addEventListener('DOMContentLoaded',()=>{const s=document.createElement('style');s.textContent='html,body{background:#1b1917 !important}';document.head.appendChild(s)})")
    page.goto(URL)
    page.wait_for_function("document.fonts.status === 'loaded'")
    page.wait_for_timeout(2500)  # first poll + artwork + playlist
    return ctx, page


with sync_playwright() as p:
    b = p.chromium.launch(channel="msedge")  # use the Edge that ships with Windows

    # 1. hero: playing, playlist open, transparent background
    ctx, page = page_for(b)
    page.screenshot(path=str(OUT / "hero.png"), omit_background=True)

    # 2. file menu open
    page.get_by_text("Tệp", exact=False).first.click()
    page.wait_for_timeout(400)
    page.screenshot(path=str(OUT / "menu.png"), omit_background=True)
    page.keyboard.press("Escape")
    page.mouse.click(300, 520)
    page.wait_for_timeout(300)

    # 3. drawer closed: only the turntable
    page.locator(".drawer").first.click()
    page.wait_for_timeout(900)
    page.screenshot(path=str(OUT / "compact.png"), omit_background=True,
                    clip={"x": 0, "y": 0, "width": 690, "height": 560})
    ctx.close()

    # 4. short clip of the spinning record on a dark backdrop
    ctx, page = page_for(b, video=True)
    page.wait_for_timeout(5000)
    vid = page.video
    ctx.close()
    webm = OUT / "_spin.webm"
    pathlib.Path(vid.path()).replace(webm)
    b.close()

# webm -> gif: skip the first 2.5 s (loading), 2.4 s loop, 640 px wide
subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-ss", "2.6", "-t", "2.4", "-i", str(webm),
                "-vf", "fps=20,crop=642:560:0:0,scale=520:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128[p];[b][p]paletteuse=dither=bayer:bayer_scale=4",
                str(OUT / "spin.gif")], check=True)
webm.unlink()
for d in (OUT / "_video",):
    if d.exists():
        for f in d.iterdir():
            f.unlink()
        d.rmdir()
print("ok", sorted(x.name for x in OUT.iterdir()))
