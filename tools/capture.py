#!/usr/bin/env python3
"""capture.py — veritable capture d'ecran du TUI, rendue en PNG (et GIF).

Le programme est lance dans un pseudo-terminal (pty.fork) a la taille voulue,
son flux ANSI est rejoue par pyte, puis chaque cellule est dessinee avec Pillow
en respectant la couleur 24 bits emise par nvtop-rs. Aucune mise en scene
manuelle : ce que rend le depot est ce que voit un terminal vrai.

    python3 tools/capture.py --out docs/screenshot.png --gif docs/nvtop-rs.gif \
        --seconds 6 --cols 132 --rows 46 -- --mask --interval 0.25

Dependencies (hors du projet, pour la doc seulement) : pip install pillow pyte
"""
import argparse
import os
import pty
import select
import struct
import fcntl
import sys
import termios
import time

import pyte
from PIL import Image, ImageDraw, ImageFont

TIOCSWINSZ = termios.TIOCSWINSZ

# palette de secours pour les noms de couleurs que pyte peut renvoyer
NAMED = {
    "default": (205, 214, 244),
    "black": (49, 50, 68),
    "red": (243, 139, 168),
    "green": (166, 227, 161),
    "brown": (249, 226, 175),
    "yellow": (249, 226, 175),
    "blue": (137, 180, 250),
    "magenta": (203, 166, 247),
    "cyan": (148, 226, 213),
    "white": (205, 214, 244),
}
BG = (24, 24, 37)
PANEL_BG = (30, 30, 46)
BORDER = (84, 86, 110)
TITLEBAR = (41, 41, 62)


def parse_color(v):
    if not v or v == "default":
        return NAMED["default"]
    try:
        return tuple(int(v[i : i + 2], 16) for i in (0, 2, 4))
    except Exception:
        return NAMED.get(v, NAMED["default"])


def run_pty(argv, cols, rows, seconds, frame_cb):
    pid, master = pty.fork()
    if pid == 0:  # enfant : un vrai terminal, comme dans une session ssh
        os.environ["TERM"] = "xterm-256color"
        os.environ["COLORTERM"] = "truecolor"
        os.environ.setdefault("LINES", str(rows))
        os.environ.setdefault("COLUMNS", str(cols))
        try:
            os.execvp(argv[0], argv)
        except Exception as e:  # pragma: no cover
            sys.stderr.write(f"exec {argv}: {e}\n")
            os._exit(127)

    fcntl.ioctl(master, TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    screen = pyte.Screen(cols, rows)
    stream = pyte.Stream(screen)
    t_end = time.time() + seconds
    last = 0.0
    try:
        while time.time() < t_end:
            r, _, _ = select.select([master], [], [], 0.1)
            if not r:
                continue
            try:
                data = os.read(master, 65536)
            except OSError:
                break
            if not data:
                break
            stream.feed(data.decode("utf-8", "replace"))
            if frame_cb and time.time() - last > 0.45:
                last = time.time()
                frame_cb(screen)
    finally:
        try:
            os.kill(pid, 15)
            os.waitpid(pid, 0)
        except Exception:
            pass
        os.close(master)
    return screen


def render(screen, font_path, bold_path, size=17, scale=2, title="nvtop-rs — moniteur GPU lu dans le pilote",
           extra_lines=0):
    cols = screen.columns
    rows = screen.lines
    f = ImageFont.truetype(font_path, size)
    fb = ImageFont.truetype(bold_path, size)
    w0 = f.getlength("M")
    ascent, descent = f.getmetrics()
    cw = int(round(w0))
    ch = ascent + descent + 2
    pad = 14
    bar = 40
    W = (cols * cw + pad * 2) * scale
    H = (rows * ch + pad * 2 + bar) * scale
    img = Image.new("RGB", (W, H), (17, 17, 27))
    d = ImageDraw.Draw(img)
    # fenetre
    r = 16 * scale
    d.rounded_rectangle([0, 0, W - 1, H - 1], radius=r, fill=PANEL_BG, outline=BORDER, width=1 * scale)
    d.rounded_rectangle([0, 0, W - 1, bar * scale], radius=r, fill=TITLEBAR)
    d.rectangle([0, bar * scale - 14 * scale, W - 1, bar * scale], fill=TITLEBAR)
    x = 20 * scale
    for c in ((243, 139, 168), (249, 226, 175), (166, 227, 161)):
        d.ellipse([x, 14 * scale, x + 14 * scale, 14 * scale + 14 * scale], fill=c)
        x += 22 * scale
    tf = ImageFont.truetype(font_path, int(size * 0.85))
    d.text((W // 2 - len(title) * 4 * scale, 12 * scale), title, font=tf, fill=(147, 153, 178))

    y0 = bar * scale + pad * scale
    for y in range(rows):
        for x in range(cols):
            cell = screen.buffer[y][x]
            chch = cell.data if cell.data != " " else " "
            if chch == " ":
                continue
            col = parse_color(cell.fg)
            fnt = fb if cell.bold else f
            d.text((pad * scale + x * cw * scale, y0 + y * ch * scale), chch, font=fnt, fill=col)
    return img


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="docs/screenshot.png")
    ap.add_argument("--gif", default=None)
    ap.add_argument("--text", default=None, help="dump du contenu texte reel du terminal")
    ap.add_argument("--seconds", type=float, default=6.0)
    ap.add_argument("--cols", type=int, default=132)
    ap.add_argument("--rows", type=int, default=46)
    ap.add_argument("--size", type=int, default=17)
    ap.add_argument("--scale", type=int, default=2)
    ap.add_argument("--title", default="nvtop-rs — moniteur GPU lu dans le pilote NVIDIA")
    ap.add_argument("--view", default=None, help="force --view=NOM")
    ap.add_argument("--bin", dest="bin", default="./target/release/nvtop-rs", help="binaire a lancer")
    ap.add_argument("extra", nargs="*", help="arguments supplementaires passes au TUI")
    a = ap.parse_args()

    argv = [a.bin] + list(a.extra)
    if "--mask" not in argv:
        argv += ["--mask"]
    if a.view:
        argv += [f"--view={a.view}"]

    font = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
    bold = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"
    frames = []

    def grab(screen):
        img = render(screen, font, bold, a.size, a.scale, a.title)
        frames.append(img)

    screen = run_pty(argv, a.cols, a.rows, a.seconds, grab if a.gif else None)
    os.makedirs(os.path.dirname(a.out) or ".", exist_ok=True)
    final = render(screen, font, bold, a.size, a.scale, a.title)
    final.save(a.out, optimize=True)
    print(f"{a.out} : {final.size[0]}x{final.size[1]}")

    if a.text:
        rows = []
        for y in range(screen.lines):
            rows.append("".join(screen.buffer[y][x].data for x in range(screen.columns)).rstrip())
        open(a.text, "w").write("\n".join(rows).rstrip() + "\n")
        print(f"{a.text} : {screen.columns}x{screen.lines}")

    if a.gif and len(frames) > 2:
        pal = [f.convert("P", palette=Image.ADAPTIVE, colors=64) for f in frames[1:]]
        frames[0].save if False else None
        pal[0].save(
            a.gif,
            save_all=True,
            append_images=pal[1:],
            duration=450,
            loop=0,
            optimize=True,
        )
        print(f"{a.gif} : {len(pal)} images")


if __name__ == "__main__":
    main()
