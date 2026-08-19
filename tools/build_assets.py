#!/usr/bin/env python3
"""
Shadowbook README sections, built to match the hero.

Design language, taken from the hero rather than invented:
  - near-black #050507 with a violet radial bloom
  - two strands: a plain white hairline (reference) and a glowing violet one (engine)
  - bloom nodes at every junction; small dots ride the strands
  - wide-tracked uppercase labels under everything, hairline vertical rules as dividers
  - Jost (geometric, Futura-derived) subset and embedded as base64 so it renders
    identically on GitHub without a network fetch

Dark only. The hero has a baked black background; a light variant underneath it
would look broken.
"""
import base64
import io
import json
import os

_HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(os.path.dirname(_HERE), "assets")
# Only needed to re-subset the font from scratch; if it is missing the cached
# subsets in FACES_CACHE are used instead, which is the normal path.
FONTDIR = os.environ.get(
    "JOST_FONTDIR", "/home/claude/fonts/node_modules/@fontsource/jost/files")
FACES_CACHE = os.path.join(_HERE, "jost-faces.json")
W = 1492  # same coordinate width as the hero, so type and strokes match 1:1

# ---- tokens (sampled from the hero PNG) -------------------------------------
BG       = "#050507"
BLOOM    = "#2a1f6b"
VIOLET   = "#8a78ff"
VIOLET_HI= "#c4b9ff"
VIOLET_LO= "#3b2f7a"
WHITE    = "#ffffff"
TEXT     = "#f4f4f7"
MUTED    = "#5b5b66"
RULE     = "#222227"
RED      = "#e5405e"

DIM      = "#565663"
FAINT    = "#3e3e49"
EASE = "cubic-bezier(.22,1,.36,1)"
CHARS = ("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
         "0123456789 .,:;/()[]-_+=<>*#%&'\"?!")


# ---- font embedding ---------------------------------------------------------
def subset_b64(weight):
    from fontTools import subset
    from fontTools.ttLib import TTFont
    f = TTFont(f"{FONTDIR}/jost-latin-{weight}-normal.woff2")
    opt = subset.Options()
    opt.flavor = "woff2"
    opt.layout_features = ["*"]
    opt.desubroutinize = True
    s = subset.Subsetter(opt)
    s.populate(text=CHARS)
    s.subset(f)
    buf = io.BytesIO()
    f.flavor = "woff2"
    f.save(buf)
    return base64.b64encode(buf.getvalue()).decode()


FACES = {}


def load_faces():
    """Subset from FONTDIR when available, else use the committed cache.

    The three subsets are deterministic, so the cache reproduces byte-identical
    SVGs on a machine that has no copy of Jost. Regenerate it by running with
    the fonts present; it is committed so this script is self-contained.
    """
    if os.path.isdir(FONTDIR):
        for w in (300, 400, 500):
            FACES[w] = subset_b64(w)
        with open(FACES_CACHE, "w", encoding="utf-8") as f:
            json.dump({str(k): v for k, v in FACES.items()}, f,
                      indent=0, sort_keys=True)
        return "subset from " + FONTDIR
    with open(FACES_CACHE, encoding="utf-8") as f:
        cached = json.load(f)
    for w in (300, 400, 500):
        FACES[w] = cached[str(w)]
    return "cached subsets from " + os.path.basename(FACES_CACHE)


def font_css():
    out = []
    for w in (300, 400, 500):
        out.append(
            f"@font-face{{font-family:'Jost';font-style:normal;font-weight:{w};"
            f"src:url(data:font/woff2;base64,{FACES[w]}) format('woff2')}}")
    out.append("text{font-family:'Jost',ui-sans-serif,-apple-system,"
               "'Segoe UI',Roboto,Helvetica,Arial,sans-serif}")
    return "".join(out)


# ---- primitives -------------------------------------------------------------
def head(h):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" '
            f'xmlns:xlink="http://www.w3.org/1999/xlink" '
            f'viewBox="0 0 {W} {h}" width="{W}" height="{h}" fill="none">')


def defs(h, blooms):
    """blooms: list of (cx%, cy%, r%, opacity)"""
    d = [f'<filter id="glow" x="-150%" y="-150%" width="400%" height="400%">'
         f'<feGaussianBlur stdDeviation="5" result="b"/><feMerge>'
         f'<feMergeNode in="b"/><feMergeNode in="b"/>'
         f'<feMergeNode in="SourceGraphic"/></feMerge></filter>',
         f'<filter id="soft" x="-150%" y="-150%" width="400%" height="400%">'
         f'<feGaussianBlur stdDeviation="9"/></filter>']
    for i, (cx, cy, r, op) in enumerate(blooms):
        d.append(f'<radialGradient id="bl{i}" cx="{cx}%" cy="{cy}%" r="{r}%">'
                 f'<stop offset="0" stop-color="{BLOOM}" stop-opacity="{op}"/>'
                 f'<stop offset="1" stop-color="{BG}" stop-opacity="0"/>'
                 f'</radialGradient>')
    d.append(f'<radialGradient id="halo"><stop offset="0" stop-color="{VIOLET}" '
             f'stop-opacity="0.55"/><stop offset="1" stop-color="{VIOLET}" '
             f'stop-opacity="0"/></radialGradient>')
    d.append(f'<radialGradient id="halow"><stop offset="0" stop-color="{WHITE}" '
             f'stop-opacity="0.45"/><stop offset="1" stop-color="{WHITE}" '
             f'stop-opacity="0"/></radialGradient>')
    d.append(f'<linearGradient id="fade" x1="0" x2="1">'
             f'<stop offset="0" stop-color="{VIOLET}" stop-opacity="0"/>'
             f'<stop offset="0.5" stop-color="{VIOLET}" stop-opacity="0.85"/>'
             f'<stop offset="1" stop-color="{VIOLET}" stop-opacity="0"/>'
             f'</linearGradient>')
    body = [f'<defs>{"".join(d)}</defs>',
            f'<rect width="{W}" height="{h}" rx="12" fill="{BG}"/>']
    for i in range(len(blooms)):
        body.append(f'<rect width="{W}" height="{h}" rx="12" fill="url(#bl{i})"/>')
    return "".join(body)


def txt(x, y, s, size=16, weight=400, fill=TEXT, anchor="middle", track=0, op=1,
        cls=None):
    # SVG letter-spacing trails the last glyph; nudge centred text back by half
    if anchor == "middle" and track:
        x = x + track / 2.0
    elif anchor == "end" and track:
        x = x + track
    c = f' class="{cls}"' if cls else ""
    o = f' opacity="{op}"' if op != 1 else ""
    t = f' letter-spacing="{track}"' if track else ""
    return (f'<text{c} x="{x:.1f}" y="{y:.1f}" font-size="{size}" '
            f'font-weight="{weight}" fill="{fill}" text-anchor="{anchor}"{t}{o}>'
            f'{s}</text>')


def label(x, y, s, fill=MUTED, size=15, track=3.4, anchor="middle", cls=None):
    return txt(x, y, s.upper(), size=size, weight=400, fill=fill, anchor=anchor,
               track=track, cls=cls)


def node(cx, cy, color=WHITE, r=4.5, halo=22, cls=None):
    h = "halow" if color == WHITE else "halo"
    c = f' class="{cls}"' if cls else ""
    return (f'<g{c}><circle cx="{cx}" cy="{cy}" r="{halo}" fill="url(#{h})"/>'
            f'<g filter="url(#glow)"><circle cx="{cx}" cy="{cy}" r="{r}" '
            f'fill="{color}"/></g></g>')


def strand(pid, d, color, wide=False, op=1.0):
    """A strand is drawn twice: a blurred bloom pass, then a hairline core."""
    bo = (0.55 if color != WHITE else 0.20) * op
    g = (f'<path d="{d}" stroke="{color}" stroke-width="{7 if wide else 5}" '
         f'fill="none" opacity="{bo:.2f}" filter="url(#soft)" '
         f'stroke-linecap="round"/>')
    return (g + f'<path id="{pid}" d="{d}" stroke="{color}" stroke-width="1.4" '
                f'fill="none" opacity="{op}" stroke-linecap="round"/>')


def rider(pid, dur, begin, r=2.4, color=WHITE):
    return (f'<circle r="{r}" fill="{color}" opacity="0.9">'
            f'<animateMotion dur="{dur}s" begin="{begin}s" repeatCount="indefinite" '
            f'calcMode="linear"><mpath href="#{pid}" xlink:href="#{pid}"/>'
            f'</animateMotion></circle>')


def pulse(cx, cy, color, dur=1.0, begin=0.0):
    return (f'<circle cx="{cx}" cy="{cy}" r="5" fill="none" stroke="{color}" '
            f'stroke-width="1.4" opacity="0">'
            f'<animate attributeName="r" values="5;30" dur="{dur}s" '
            f'begin="{begin}s" repeatCount="indefinite"/>'
            f'<animate attributeName="opacity" values="0.6;0" dur="{dur}s" '
            f'begin="{begin}s" repeatCount="indefinite"/></circle>')


def check(x, y, color, s=1.0):
    return (f'<path d="M{x},{y} l{7*s},{8*s} l{15*s},{-18*s}" stroke="{color}" '
            f'stroke-width="{2.6*s}" fill="none" stroke-linecap="round" '
            f'stroke-linejoin="round" filter="url(#glow)"/>')


def counter(name, steps, dur, delay, x, y, size, color, track=0):
    n = len(steps)
    css, body = [], []
    for i, s in enumerate(steps):
        p0, p1 = i * 100.0 / n, (i + 1) * 100.0 / n
        k = f"{name}{i}"
        if n == 1:
            fr = "0%{opacity:0}100%{opacity:1}"
        elif i == 0:
            fr = (f"0%{{opacity:1}}{p1:.3f}%{{opacity:1}}"
                  f"{p1+0.001:.3f}%{{opacity:0}}100%{{opacity:0}}")
        elif i == n - 1:
            fr = (f"0%{{opacity:0}}{p0:.3f}%{{opacity:0}}"
                  f"{p0+0.001:.3f}%{{opacity:1}}100%{{opacity:1}}")
        else:
            fr = (f"0%{{opacity:0}}{p0:.3f}%{{opacity:0}}{p0+0.001:.3f}%{{opacity:1}}"
                  f"{p1:.3f}%{{opacity:1}}{p1+0.001:.3f}%{{opacity:0}}100%{{opacity:0}}")
        css.append(f"@keyframes {k}{{{fr}}}")
        css.append(f".{k}{{animation:{k} {dur}s linear {delay}s 1 forwards}}")
        body.append(txt(x, y, s, size=size, weight=400, fill=color, track=track,
                        cls=f"num {k}"))
    return "".join(css), "".join(body)


# =============================================================== 1. HIGHLIGHTS
def highlights():
    H = 306
    cols = [
        (["0", "100", "10,000", "1,204,000", "18,600,000", "62,400,000",
          "94,100,000", "100,000,000"], "operations verified",
         "seed 42 · reference vs engine", WHITE, 1.15),
        (["0"], "divergences", "not one disagreement", VIOLET, 0.5),
        (["0ns", "46ns", "195ns", "325ns", "387ns", "410ns", "416ns"],
         "p50 insert, no cross", "p99 758ns · p99.9 1,593ns", VIOLET, 1.0),
        (["0"], "hot-path allocations", "arena + preallocated band", VIOLET, 0.5),
    ]
    css = [".num{opacity:0}", "@keyframes dr{to{stroke-dashoffset:0}}",
           "@keyframes fi{to{opacity:1}}"]
    s = [defs(H, [(18, 60, 55, 0.5), (82, 40, 45, 0.28)])]
    cw = W / 4.0
    for i, (steps, lab, sub, col, dur) in enumerate(cols):
        cx = cw * i + cw / 2
        delay = 0.08 + i * 0.14
        c, b = counter(f"h{i}n", steps, dur, delay, cx, 138, 46, col, track=1)
        css.append(c)
        s.append(b)
        s.append(label(cx, 176, lab, size=15, track=3.4, cls=f"fx{i}"))
        s.append(txt(cx, 206, sub, size=15, weight=300, fill=DIM,
                     track=0.6, cls=f"fx{i}"))
        css.append(f".fx{i}{{opacity:0;animation:fi 0.6s ease {delay+dur*0.75:.2f}s "
                   f"1 forwards}}")
        # hairline that fills, with a bloom node riding to the end
        x1, x2, y = cx - 150, cx + 150, 240
        s.append(f'<line x1="{x1}" y1="{y}" x2="{x2}" y2="{y}" stroke="{RULE}" '
                 f'stroke-width="1.2"/>')
        s.append(f'<line class="ul{i}" x1="{x1}" y1="{y}" x2="{x2}" y2="{y}" '
                 f'stroke="{col}" stroke-width="1.2" stroke-dasharray="300" '
                 f'stroke-dashoffset="300" filter="url(#soft)" opacity="0.9"/>')
        s.append(f'<line class="ul{i}" x1="{x1}" y1="{y}" x2="{x2}" y2="{y}" '
                 f'stroke="{col}" stroke-width="1.2" stroke-dasharray="300" '
                 f'stroke-dashoffset="300"/>')
        css.append(f".ul{i}{{animation:dr 0.9s {EASE} {delay}s 1 forwards}}")
        s.append(f'<g class="nd{i}">{node(x2, y, col, r=3, halo=13)}</g>')
        css.append(f".nd{i}{{opacity:0;animation:fi 0.4s ease {delay+0.9:.2f}s "
                   f"1 forwards}}")
        if i:
            s.append(f'<line x1="{cw*i}" y1="86" x2="{cw*i}" y2="222" '
                     f'stroke="{RULE}" stroke-width="1"/>')
    return head(H) + f"<style>{font_css()}{''.join(css)}</style>" + "".join(s) + "</svg>"



# ============================================================= 2. ARCHITECTURE
def architecture():
    H = 476
    s = [defs(H, [(50, 18, 50, 0.42), (26, 88, 42, 0.3)])]
    edges = [
        ("a0", "M746,108 C746,172 420,150 420,224", WHITE, 0.5),
        ("a1", "M746,108 C746,172 1072,150 1072,224", VIOLET, 1.0),
        ("a2", "M420,260 C420,332 640,318 640,378", WHITE, 0.5),
        ("a3", "M1072,260 C1072,350 700,330 646,378", VIOLET, 1.0),
        ("a4", "M1072,260 C1072,332 1180,318 1180,378", VIOLET, 0.45),
    ]
    for pid, d, c, op in edges:
        s.append(strand(pid, d, c, op=op))
    for i, (pid, _, c, _) in enumerate(edges):
        base = 0.0 if i < 2 else 0.8
        for k in range(2):
            s.append(rider(pid, 3.0, base + k * 1.5, 2.2,
                           VIOLET_HI if c == VIOLET else WHITE))
    # every label sits clear of the strand leaving its node
    nodes = [
        (746, 84, "types/", "shared vocabulary", VIOLET, "above", 5, 26),
        (420, 242, "reference/", "obviously correct", WHITE, "w", 5, 26),
        (1072, 242, "engine/", "obviously fast", VIOLET, "e", 5, 26),
        (640, 396, "fuzz/", "differential harness", VIOLET, "below", 7, 34),
        (1180, 396, "benches/", "tail latency", WHITE, "below", 4, 18),
    ]
    for cx, cy, name, sub, col, side, r, halo in nodes:
        s.append(node(cx, cy, col, r=r, halo=halo))
        if side == "w":
            s.append(txt(cx - 30, cy - 2, name, size=22, weight=400, fill=TEXT,
                         anchor="end"))
            s.append(label(cx - 30, cy + 22, sub, size=12.5, track=2.6, anchor="end"))
        elif side == "e":
            s.append(txt(cx + 30, cy - 2, name, size=22, weight=400, fill=TEXT,
                         anchor="start"))
            s.append(label(cx + 30, cy + 22, sub, size=12.5, track=2.6, anchor="start"))
        elif side == "above":
            s.append(txt(cx, cy - 48, name, size=22, weight=400, fill=TEXT))
            s.append(label(cx, cy - 24, sub, size=12.5, track=2.6))
        else:
            s.append(txt(cx, cy + 38, name, size=22, weight=400, fill=TEXT))
            s.append(label(cx, cy + 60, sub, size=12.5, track=2.6))
    s.append(label(60, 46, "architecture", fill=DIM, size=14, track=5, anchor="start"))
    return head(H) + f"<style>{font_css()}</style>" + "".join(s) + "</svg>"


# ============================================================== 3. VERIFICATION
def verification():
    H = 436
    cy, ty, by = 232, 136, 328
    top = f"M188,{cy} C400,{cy} 380,{ty} 640,{ty} L980,{ty} C1180,{ty} 1120,{cy} 1268,{cy}"
    bot = f"M188,{cy} C400,{cy} 380,{by} 640,{by} L980,{by} C1180,{by} 1120,{cy} 1268,{cy}"
    s = [defs(H, [(14, 55, 52, 0.5), (86, 55, 40, 0.34)])]
    s.append(f'<line x1="188" y1="88" x2="188" y2="{cy}" stroke="{FAINT}" '
             f'stroke-width="1.2"/>')
    s.append(strand("vt", top, WHITE, op=0.7))
    s.append(strand("vb", bot, VIOLET, op=1.0))
    for b in (0.0, 1.1, 2.2):
        s.append(rider("vt", 3.3, b, 2.8, WHITE))
        s.append(rider("vb", 3.3, b, 2.8, VIOLET_HI))
    s.append(node(188, cy, WHITE, r=5, halo=26))
    s.append(label(188, cy + 48, "command", size=13, track=3))
    # name first, descriptor second, both clear of the strand's bloom
    s.append(txt(810, 78, "reference/", size=22, weight=400, fill=TEXT))
    s.append(label(810, 102, "slow, boring, obviously right", size=12.5, track=2.6,
                   fill=DIM))
    s.append(txt(810, 368, "engine/", size=22, weight=400, fill=TEXT))
    s.append(label(810, 392, "arena-backed, zero hot-path alloc", size=12.5,
                   track=2.6, fill=DIM))
    s.append(pulse(1268, cy, WHITE, 1.1, begin=3.3))
    s.append(node(1268, cy, VIOLET, r=5, halo=26))
    s.append(check(1308, cy - 2, WHITE, 1.1))
    s.append(label(1312, cy + 48, "assert equal", size=13, track=3, fill=MUTED))
    s.append(label(60, 46, "verification", fill=DIM, size=14, track=5, anchor="start"))
    return head(H) + f"<style>{font_css()}</style>" + "".join(s) + "</svg>"


# ================================================================ 4. BENCHMARK
def benchmark():
    H = 344
    x0, x1, ay = 120, 1372, 176
    # Axis top is chosen to clear the widest mark; ticks are quarters of it.
    axis_max = 2000
    px = lambda ns: x0 + ns * (x1 - x0) / float(axis_max)
    marks = [(416, "p50", VIOLET, 46, 5.5), (758, "p99", WHITE, 32, 4),
             (1593, "p99.9", WHITE, 32, 4)]
    css = ["@keyframes mk{from{opacity:0;transform:translateY(18px)}"
           "to{opacity:1;transform:translateY(0)}}",
           "@keyframes dr{to{stroke-dashoffset:0}}"]
    s = [defs(H, [(28, 45, 50, 0.42)])]
    s.append(label(60, 46, "benchmarks", fill=DIM, size=14, track=5, anchor="start"))
    s.append(txt(1432, 46, "insert, no cross", size=17, weight=300, fill=MUTED,
                 anchor="end"))
    s.append(f'<line x1="{x0}" y1="{ay}" x2="{x1}" y2="{ay}" stroke="{RULE}" '
             f'stroke-width="1.2"/>')
    ln = px(marks[-1][0]) - x0
    s.append(f'<line class="ax" x1="{x0}" y1="{ay}" x2="{px(marks[-1][0])}" y2="{ay}" '
             f'stroke="{VIOLET_LO}" stroke-width="1.4" stroke-dasharray="{ln}" '
             f'stroke-dashoffset="{ln}"/>')
    css.append(f".ax{{animation:dr 1.1s {EASE} 0.1s 1 forwards}}")
    for ns in [axis_max * i // 4 for i in range(5)]:
        s.append(f'<line x1="{px(ns)}" y1="{ay-5}" x2="{px(ns)}" y2="{ay+5}" '
                 f'stroke="{RULE}" stroke-width="1"/>')
        s.append(txt(px(ns), ay - 18, f"{ns}", size=13, weight=300, fill=FAINT))
    for i, (ns, lab, col, size, r) in enumerate(marks):
        x = px(ns)
        s.append(f'<g class="m{i}">'
                 f'<line x1="{x}" y1="{ay}" x2="{x}" y2="{ay+50}" stroke="{RULE}" '
                 f'stroke-width="1"/>'
                 f'{node(x, ay, col, r=r/1.4, halo=20 if i==0 else 13)}'
                 f'{txt(x, ay+80, f"{ns:,}ns", size=size, weight=400, fill=col if i==0 else TEXT, track=0.8)}'
                 f'{label(x, ay+104, lab, size=13, track=3)}</g>')
        css.append(f".m{i}{{opacity:0;animation:mk 0.8s {EASE} "
                   f"{0.25+i*0.18:.2f}s 1 forwards}}")
    s.append(txt(120, 322, "2,000,000 samples \u00b7 median of 7 runs \u00b7 single-threaded "
                 "\u00b7 live churn on a 65,536-tick band and 65,536-order arena after 1M "
                 "warmup commands \u00b7 i5-12450HX, Ubuntu 26.04, rustc 1.95.0 "
                 "\u00b7 hdrhistogram + Instant, no bench harness "
                 "\u00b7 no pinning or governor control",
                 size=13.5, weight=300, fill=DIM, anchor="start"))
    return head(H) + f"<style>{font_css()}{''.join(css)}</style>" + "".join(s) + "</svg>"


# ================================================================= 5. FINDINGS
def findings():
    """One bug, told once per loop, with causality.

    The dot pauses on WRONG (fraction 0.2753 of the strand) and the node flashes
    as it lands; the ring at the merge fires only when both dots arrive. Nothing
    animates ambiently. Palette is the hero's: white, violet, plus a single red
    kept below the violet in luminance so it reads as semantic, not decorative.
    """
    H, T, FW = 536, 4.0, 0.2753
    lx, rx, mx = 470, 1022, 746
    kt = "0;0.30;0.375;0.80;1"
    kp = f"0;{FW};{FW};1;1"
    s = [defs(H, [(31, 30, 44, 0.4), (69, 30, 44, 0.4), (50, 76, 40, 0.3)])]
    s.append(label(60, 46, "what 100,000,000 operations bought", fill=DIM,
                   size=14, track=5, anchor="start"))
    paths = [("f0", f"M{lx},128 L{lx},248 C{lx},342 {mx-116},328 {mx-6},370", WHITE, 0.7),
             ("f1", f"M{rx},128 L{rx},248 C{rx},342 {mx+116},328 {mx+6},370", VIOLET, 1.0)]
    for pid, d, c, op in paths:
        s.append(strand(pid, d, c, op=op))
    for pid, _, c, _ in paths:
        s.append(f'<circle r="3.2" fill="{VIOLET_HI if c == VIOLET else WHITE}">'
                 f'<animateMotion dur="{T}s" repeatCount="indefinite" '
                 f'calcMode="linear" keyPoints="{kp}" keyTimes="{kt}">'
                 f'<mpath href="#{pid}" xlink:href="#{pid}"/></animateMotion></circle>')
    s.append(f'<line x1="{lx+34}" y1="128" x2="{rx-34}" y2="128" stroke="{VIOLET_LO}" '
             f'stroke-width="1.2" stroke-dasharray="4 6"/>')
    s.append(txt(mx, 116, "cancel(id 7)", size=19, weight=300, fill=TEXT))
    s.append(label(mx, 156, "id reused \u00b7 same assumption", fill=VIOLET, size=13,
                   track=3))
    for cx, name, col, side in ((lx, "reference/", WHITE, -1), (rx, "engine/", VIOLET, 1)):
        s.append(txt(cx, 90, name, size=21, weight=400, fill=TEXT))
        s.append(node(cx, 128, col, r=4, halo=20))
        # node and label flash together, on the beat the dot arrives
        s.append(f'<g opacity="0.42"><animate attributeName="opacity" dur="{T}s" '
                 f'repeatCount="indefinite" values="0.42;0.42;1;1;0.42;0.42" '
                 f'keyTimes="0;0.29;0.31;0.40;0.52;1"/>'
                 f'{node(cx, 248, RED, r=5, halo=24)}'
                 f'<g filter="url(#glow)">'
                 f'{txt(cx + side*32, 256, "WRONG", size=22, weight=500, fill=RED, track=3.6, anchor="end" if side<0 else "start")}'
                 f'</g></g>')
    # the ring fires at 0.80 of the cycle: the instant both dots land
    s.append(f'<circle cx="{mx}" cy="370" r="5" fill="none" stroke="{WHITE}" '
             f'stroke-width="1.4" opacity="0">'
             f'<animate attributeName="r" values="5;5;5;30;30" '
             f'keyTimes="0;0.79;0.80;0.98;1" dur="{T}s" repeatCount="indefinite"/>'
             f'<animate attributeName="opacity" values="0;0;0.75;0;0" '
             f'keyTimes="0;0.79;0.80;0.98;1" dur="{T}s" repeatCount="indefinite"/>'
             f'</circle>')
    s.append(node(mx, 370, VIOLET, r=4.5, halo=22))
    s.append(check(mx + 30, 368, WHITE, 1.0))
    s.append(label(mx, 414, "equal \u00b7 suite passes", size=13, track=3, fill=MUTED))
    s.append(f'<g filter="url(#glow)">'
             f'{txt(mx, 466, "The verifier cannot detect shared assumptions.", size=38, weight=300, fill=TEXT, track=0.6)}'
             f'</g>')
    s.append(txt(mx, 500, "Both sides are wrong. The suite still passes.",
                 size=17, weight=300, fill=MUTED, track=0.6))
    return head(H) + f"<style>{font_css()}</style>" + "".join(s) + "</svg>"


SECTIONS = {"highlights": highlights, "architecture": architecture,
            "verification": verification, "benchmark": benchmark,
            "findings": findings}

if __name__ == "__main__":
    print(load_faces())
    os.makedirs(OUT, exist_ok=True)
    for name, fn in SECTIONS.items():
        p = os.path.join(OUT, f"{name}.svg")
        with open(p, "w", encoding="utf-8") as f:
            f.write(fn())
        print(f"{name:14s} {os.path.getsize(p)/1024:6.1f} KB")
