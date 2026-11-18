#!/usr/bin/env python3
"""Wisp TUI brand preview: the palette, the mark, every animation and a styled chat turn.

    python3 docs/design/preview.py                    everything, about 80 seconds (any key skips, q quits)
    python3 docs/design/preview.py --list             the section names
    python3 docs/design/preview.py --section orb      one section (repeat --section for more)
    python3 docs/design/preview.py --reduced-motion   what reduced motion looks like
    python3 docs/design/preview.py --theme light --colors 256 --ascii

Python 3.8+ and the standard library only. NO_COLOR, COLORTERM, TERM, COLORFGBG, WISP_MOTION, WISP_THEME and
WISP_COLOR are honoured, and flags win over the environment. When stdout isn't a terminal the preview prints
static frames and never waits, queries or moves the cursor.

The spec is docs/design/tui-brand.md. Its token, glyph, frame, curve and sampling-grid tables are read from it at
start; the rest is copied from its prose. Where the two disagree, the spec wins and this file has a bug.
"""

import argparse
import math
import os
import re
import select
import shutil
import signal
import sys
import time
import unicodedata

try:  # POSIX only; on native Windows the preview still runs, without probes or key polling
    import termios
    import tty
except ImportError:  # pragma: no cover
    termios = tty = None


# ════════════════════════════════════════════ colour math ════════════════════════════════════════════

def rgb(h):
    h = h.lstrip('#')
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def to_hex(c):
    return '#%02X%02X%02X' % tuple(max(0, min(255, round(v))) for v in c)


def mix(a, b, t):
    """t of colour a over colour b in sRGB: CSS color-mix(in srgb), which is how the orb engine applies opacity."""
    t = max(0.0, min(1.0, t))
    A, B = rgb(a), rgb(b)
    return to_hex(tuple(B[i] + (A[i] - B[i]) * t for i in range(3)))


def _lin(v):
    v /= 255
    return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4


def _delin(v):
    v = max(0.0, min(1.0, v))
    return 255 * (12.92 * v if v <= 0.0031308 else 1.055 * v ** (1 / 2.4) - 0.055)


def luminance(h):
    r, g, b = (_lin(x) for x in rgb(h))
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(a, b):
    la, lb = luminance(a), luminance(b)
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)


def oklab(h):
    r, g, b = (_lin(x) for x in rgb(h))
    l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b
    m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b
    s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b
    l, m, s = (math.copysign(abs(v) ** (1 / 3), v) for v in (l, m, s))
    return (0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
            1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
            0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s)


def oklch(h):
    L, a, b = oklab(h)
    return L, math.hypot(a, b), math.degrees(math.atan2(b, a)) % 360


def from_oklch(L, C, H):
    """OKLCH to hex, reducing chroma until the colour fits in sRGB."""
    for _ in range(120):
        a, b = C * math.cos(math.radians(H)), C * math.sin(math.radians(H))
        l_ = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3
        m_ = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3
        s_ = (L - 0.0894841775 * a - 1.2914855480 * b) ** 3
        lin = (4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
               -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
               -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_)
        if all(-1e-4 <= v <= 1 + 1e-4 for v in lin):
            break
        C *= 0.97
    return to_hex(tuple(_delin(v) for v in lin))


def fit(h, bg, target):
    """The spec's runtime correction: move OKLCH lightness away from the background until contrast >= target."""
    if contrast(h, bg) >= target:
        return h
    L, C, H = oklch(h)
    step = 0.004 if oklab(bg)[0] < 0.6 else -0.004
    while 0.0 <= L <= 1.0:
        L += step
        cand = from_oklch(L, C, H)
        if contrast(cand, bg) >= target:
            return cand
    return h


BAND_PAIR = ('#252526', '#1E1E1E')
FIT_STEPS = 10000


def fitted_band(over, under):
    """The theme's band: the first of FIT_STEPS steps of over into under that stands as far off under as BAND_PAIR."""
    target = contrast(*BAND_PAIR)
    for k in range(1, FIT_STEPS + 1):
        band = mix(over, under, k / FIT_STEPS)
        if contrast(band, under) >= target:
            return band
    return over


def _xterm():
    lv = [0, 95, 135, 175, 215, 255]
    out = {16 + i: to_hex((lv[i // 36], lv[(i // 6) % 6], lv[i % 6])) for i in range(216)}
    out.update({232 + i: to_hex((8 + 10 * i,) * 3) for i in range(24)})
    return out


XTERM = _xterm()
_N256 = {}


def nearest256(h):
    """Nearest xterm colour in 16-255 (0-15 depend on the user's theme), by OKLab distance."""
    if h not in _N256:
        L0, a0, b0 = oklab(h)
        _N256[h] = min(XTERM, key=lambda i: sum((p - q) ** 2 for p, q in zip(oklab(XTERM[i]), (L0, a0, b0))))
    return _N256[h]


# ════════════════════════════════════════════ tokens (spec §2) ════════════════════════════════════════════
# Read from the spec's §2.2, §2.4 and §2.5 tables, so the preview can never drift from them; §5 and §6.2 below.

SPEC = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'tui-brand.md')
MODES = ('dark', 'light')


def _tables(spec, start, end, headers):
    """The Markdown tables between two headings, as rows of cells; stops on a header row other than the expected."""
    i = spec.index(start)
    lines = [line.strip() for line in spec[i:spec.index(end, i)].splitlines()]
    out = []
    for n, line in enumerate(lines[:-1]):
        if line.startswith('|') and lines[n + 1].startswith('|---'):
            head = _cells(line)
            if len(out) >= len(headers) or head != headers[len(out)]:
                sys.exit('preview.py: %s has an unexpected table: %s' % (start, line))
            rows = []
            for row in lines[n + 2:]:
                if not row.startswith('|'):
                    break
                rows.append(_cells(row))
            out.append(rows)
    if len(out) != len(headers):
        sys.exit('preview.py: %s has %d tables, not %d' % (start, len(out), len(headers)))
    return out


def _cells(row):
    """A table row's cells, split as GFM does: a pipe after a backslash is part of the cell, even inside code."""
    return [c.strip().replace('\\|', '|') for c in re.split(r'(?<!\\)\|', row.strip()[1:-1])]


def _spans(cell):
    """The text of each code span in a cell, `` `- `` included."""
    return [m.group(2)[1:-1] if m.group(2)[:1] == m.group(2)[-1:] == ' ' and m.group(2).strip() else m.group(2)
            for m in re.finditer(r'(`+)(.+?)(?<!`)\1(?!`)', cell)]


def _names(cell, known=()):
    """The token groups a first cell names: `a` / `b`, `c`, with `x*` naming every known token that starts with x."""
    groups = []
    for part in cell.split(' / '):
        names = []
        for name in re.findall(r'`([^`]+)`', part):
            names += [k for k in known if k.startswith(name[:-1])] if name.endswith('*') else [name]
        groups.append(names)
    return groups


def _read_spec(spec):
    palette, aliases, floors = {m: {} for m in MODES}, {}, {}
    heads = [['Token', 'Dark', 'Light', 'Use', 'Min'], ['Token', 'Dark', 'Light', 'Recipe', 'Min'],
             ['Token', 'Dark', 'Light', 'Use']]
    for table, head in zip(_tables(spec, '### 2.2 Tokens', '### 2.3', heads), heads):
        for row in table:
            groups = _names(row[0])
            if row[1].startswith('='):
                for group, target in zip(groups, _names(row[1])):
                    aliases.update((name, target[0]) for name in group)
            else:
                for mode, cell in zip(MODES, row[1:3]):
                    for group, value in zip(groups, re.findall(r'#[0-9A-Fa-f]{6}', cell)):
                        palette[mode].update((name, value) for name in group)
            if head[-1] == 'Min' and re.match(r'\d', row[-1]):
                floors.update((name, float(row[-1])) for group in groups for name in group)
    known = sorted(set(palette['dark']) | set(aliases))
    ansi256, ansi16 = {m: {} for m in MODES}, {m: {} for m in MODES}
    for start, end, head, out, read in (
            ('### 2.4 ANSI-256', '### 2.5', ['Token', 'Dark', 'Light'], ansi256, int),
            ('### 2.5 ANSI-16', '### 2.6', ['Token', 'Dark background', 'Light background'], ansi16,
             lambda v: None if v.startswith('default') else int(v))):
        for row in _tables(spec, start, end, [head])[0]:
            for mode, cell in zip(MODES, row[1:3] if '`' in row[0] else ()):
                for group, part in zip(_names(row[0], known), cell.split(' / ')):
                    value = read(re.match(r'default|\d+', part.strip()).group())
                    out[mode].update((name, value) for name in group)
    return palette, aliases, floors, ansi256, ansi16


SPEC_TEXT = open(SPEC, encoding='utf-8').read()
PALETTE, ALIASES, MIN_CONTRAST, ANSI256, ANSI16 = _read_spec(SPEC_TEXT)


def resolve_tokens(theme, probed_bg=None, probed_fg=None):
    """Stone verbatim, or (when the real background is known and differs) corrected and re-derived (spec §2.5)."""
    tok = dict(PALETTE[theme])
    for k, v in ALIASES.items():
        tok[k] = tok[v]
    if not probed_bg:
        return tok, False
    tok['surface'] = fitted_band(tok['text'], probed_bg)
    L0, a0, b0 = oklab(PALETTE[theme]['bg'])
    L1, a1, b1 = oklab(probed_bg)
    if math.sqrt((L0 - L1) ** 2 + (a0 - a1) ** 2 + (b0 - b1) ** 2) <= 0.03:
        return tok, False
    bg = probed_bg
    fg = probed_fg or tok['text']
    tok['bg'], tok['text'] = bg, fg
    for k, need in MIN_CONTRAST.items():
        if k != 'text':
            tok[k] = fit(tok[k], bg, need)
    dark = theme == 'dark'
    tok['sunken'] = mix('#000000', bg, 0.35) if dark else mix(fg, bg, 0.05)
    tok['line'] = mix(fg, bg, 0.24 if dark else 0.30)
    tok['accent.tint'] = mix(tok['accent'], bg, 0.22 if dark else 0.20)
    tok['warn.tint'] = mix(tok['warn.fill'], bg, 0.14)
    tok['diff.add'] = mix(tok['ok'], bg, 0.14)
    tok['diff.del'] = mix(tok['error'], bg, 0.14)
    tok['diff.add.word'] = mix(tok['ok'], bg, 0.26)
    tok['diff.del.word'] = mix(tok['error'], bg, 0.26)
    tok['orb.ring'] = fit(mix(fg, bg, 0.55), bg, 3.0)
    tok['orb.ring.listen'] = mix(fg, bg, 0.85)
    tok['orb.ring.speak'] = mix(fg, bg, 0.40)
    tok['orb.ring.dim'] = mix(fg, bg, 0.30)
    tok['user'] = tok['orb.ring']
    for k, v in ALIASES.items():
        tok[k] = tok[v]
    return tok, True


# ════════════════════════════════════════════ glyphs (spec §5) ════════════════════════════════════════════

# The glyphs the preview draws or measures, by their §5.1 Key.
GLYPH_KEYS = ('prompt', 'user', 'wisp', 'caret', 'comet', 'running', 'done', 'failed', 'waiting', 'stopped',
              'needs_you', 'rail', 'approval_rail', 'collapsed', 'expanded', 'separator', 'bullet', 'more', 'arrow',
              'minus', 'tee', 'elbow', 'pipe', 'cell_full', 'cell_free', 'cell_reserve')


def _glyphs(spec):
    """The Unicode, ASCII and ambiguous-wide sets from §5.1; the last holds only what differs from Unicode."""
    head = ['Meaning', 'Unicode', 'Code point', 'Width', 'Ambiguous-wide set', 'ASCII', 'Colour', 'Key']
    sets = {}, {}, {}
    for row in _tables(spec, '### 5.1 Glyphs', '### 5.2', [head])[0]:
        uni = _spans(row[1])
        wide = uni if row[4] == 'same' else _spans(row[4])
        cols = uni, wide, wide if row[5].startswith('same') else _spans(row[5])
        for n, key in enumerate(_spans(row[7])):
            for out, col in zip(sets, cols):
                out[key] = col[n]
    unicode, wide, ascii_ = ({k: out[k] for k in GLYPH_KEYS} for out in sets)
    return unicode, ascii_, {k: v for k, v in wide.items() if v != unicode[k]}


# When the terminal draws East Asian "ambiguous" characters two cells wide, NARROW replaces the Unicode set's
# ambiguous glyphs (spec §5.1). Everything else in the Unicode set is already narrow.
UNICODE, ASCII, NARROW = _glyphs(SPEC_TEXT)

# The presence orb ("dock orb"), three cells on a 6 x 4 braille dot grid (spec §3.1, §6.3).
# Ring lights R0..R7, clockwise from the top left; the core is the middle cell, U+2836.
RING = [(1, 0), (4, 0), (5, 1), (5, 2), (4, 3), (1, 3), (0, 2), (0, 1)]
BIT = {(0, 0): 0x01, (0, 1): 0x02, (0, 2): 0x04, (1, 0): 0x08, (1, 1): 0x10, (1, 2): 0x20, (0, 3): 0x40,
       (1, 3): 0x80}
ALL = frozenset(range(8))
REST = ALL - {1}                  # the logo's open ring: the gap sits at 1-2 o'clock
CORE = UNICODE['wisp']
_FRAMES = {row[0]: row[1] for row in _tables(SPEC_TEXT, '### 5.2 The frames', '### 5.3', [
    ['Animation', 'Frames', 'Code points', 'Timing']])[0]}
CHECK = tuple(_spans(_FRAMES['Orb done'])[0])   # the ring redrawn as a check
COMET = UNICODE['comet']                # one light and its tail, circling a single cell clockwise
COMET_ASCII = ASCII['comet']


def ring_cells(on):
    left = right = 0
    for i in on:
        x, y = RING[i]
        if x < 2:
            left |= BIT[(x % 2, y)]
        else:
            right |= BIT[(x % 2, y)]
    # An empty half is a plain space, never U+2800, so copied text stays clean (spec §3.1)
    return (chr(0x2800 + left) if left else ' '), (chr(0x2800 + right) if right else ' ')


# The splash orb, 8 x 3 cells: 18 ring lights (two hidden in the gap) and a 12-dot core (spec §3.2).
SPLASH_REST = [' ⡐⠈ ⠁⠁  ', '⠰  ⢾⡷  ⠆', ' ⠡⢀ ⡀⡀⠌ ']
SPLASH_CELLS = {  # (col, row): ring light indices k (0 = 12 o'clock, clockwise, 20 degrees apart)
    (1, 0): (15, 16), (2, 0): (17,), (4, 0): (0,), (5, 0): (1,), (0, 1): (13, 14), (7, 1): (4, 5),
    (1, 2): (11, 12), (2, 2): (10,), (4, 2): (9,), (5, 2): (8,), (6, 2): (6, 7)}
SPLASH_LIGHT_BITS = {  # light k -> (col, row, bit)
    0: (4, 0, 0x01), 1: (5, 0, 0x01), 4: (7, 1, 0x02), 5: (7, 1, 0x04), 6: (6, 2, 0x04), 7: (6, 2, 0x08),
    8: (5, 2, 0x40), 9: (4, 2, 0x40), 10: (2, 2, 0x80), 11: (1, 2, 0x20), 12: (1, 2, 0x01), 13: (0, 1, 0x20),
    14: (0, 1, 0x10), 15: (1, 0, 0x40), 16: (1, 0, 0x10), 17: (2, 0, 0x08)}
SPLASH_CORE = {(3, 1): '⢾', (4, 1): '⡷'}
SPLASH_ASCII = [" .-''  .", ":  **  :", " '-..-' "]


# ════════════════════════════════════════════ easing (spec §6.2) ════════════════════════════════════════════

def bezier(x1, y1, x2, y2):
    def f(x):
        if x <= 0:
            return 0.0
        if x >= 1:
            return 1.0
        lo, hi = 0.0, 1.0
        for _ in range(40):
            t = (lo + hi) / 2
            bx = 3 * (1 - t) ** 2 * t * x1 + 3 * (1 - t) * t ** 2 * x2 + t ** 3
            lo, hi = (t, hi) if bx < x else (lo, t)
        t = (lo + hi) / 2
        return 3 * (1 - t) ** 2 * t * y1 + 3 * (1 - t) * t ** 2 * y2 + t ** 3
    return f


def _motion(spec):
    """§6.2's sampling grid in ms, by the first word of each animation, and its cubic-bezier curves by name."""
    grid, _, curves = _tables(spec, '### 6.2 Clock', '### 6.3', [['Animation', 'Grid'], ['Demo', 'Rate'],
                                                                  ['Name', 'Curve', 'Used for']])
    return ({row[0].split()[0].rstrip(':'): int(re.match(r'\d+', row[1]).group()) for row in grid},
            {_spans(row[0])[0]: bezier(*map(float, row[1].strip('()').split(','))) for row in curves
             if row[1].startswith('(')})


GRID, CURVES = _motion(SPEC_TEXT)
EASE_IN_OUT = CURVES['ease-in-out']   # loops
EASE_OUT = CURVES['ease-out']         # decays
MORPH = CURVES['morph']               # the orb engine's state morph
BEAT = CURVES['beat']                 # the reserved "needs you" beat


def q(t, step):
    """Sample time on a grid: brightness loops change at most once per step, so frames are only written then."""
    return t - t % step


def wave(t, period):
    """0 -> 1 -> 0 over one period, sine-shaped (an ease-in-out loop)."""
    return 0.5 - 0.5 * math.cos(2 * math.pi * ((t % period) / period))


def keyframes(t, frames, ease):
    """Sample [(ms, value), ...] at t with an easing curve per segment."""
    if t <= frames[0][0]:
        return frames[0][1]
    for (t0, v0), (t1, v1) in zip(frames, frames[1:]):
        if t <= t1:
            return v0 + (v1 - v0) * ease((t - t0) / (t1 - t0))
    return frames[-1][1]


BEAT_KEYS = [(0, 0.5), (176, 1.0), (352, 0.6), (528, 0.95), (768, 0.5), (1600, 0.5)]


# ════════════════════════════════════════════ terminal ════════════════════════════════════════════

class Caps:
    tty = False          # stdout is a terminal
    keys = False         # stdin is a terminal we can poll
    depth = 'truecolor'  # truecolor | 256 | 16 | none
    theme = 'dark'
    bg = None
    fg = None
    sync = False
    sync_reply = 'not asked'
    ascii = False
    amb_wide = False
    quick = False
    motion = 'full'
    width = 80


class Term:
    """Owns every terminal mode the preview changes, and puts each one back (spec §9.6)."""

    def __init__(self, caps):
        self.caps = caps
        self.saved = None
        self.out = sys.stdout

    def __enter__(self):
        if self.caps.keys and termios:
            fd = sys.stdin.fileno()
            self.saved = termios.tcgetattr(fd)
            tty.setcbreak(fd)          # no echo, no line buffering; Ctrl-C still raises KeyboardInterrupt
        if self.caps.tty:
            self.write('\x1b[?25l')    # hide the cursor while animating
        return self

    def restore(self):
        if self.caps.tty:
            self.write('\x1b[0m\x1b[?2026l\x1b[?25h')
            self.out.flush()
        if self.saved is not None:
            termios.tcsetattr(sys.stdin.fileno(), termios.TCSADRAIN, self.saved)
            self.saved = None

    def __exit__(self, *exc):
        self.restore()
        return False

    def write(self, s):
        self.out.write(s)

    def flush(self):
        self.out.flush()

    def frame(self, s):
        """One frame, held by the terminal until complete where DEC 2026 is supported."""
        if self.caps.sync:
            s = '\x1b[?2026h' + s + '\x1b[?2026l'
        self.out.write(s)
        self.out.flush()

    def key(self, timeout=0.0):
        if not self.caps.keys:
            if timeout:
                time.sleep(timeout)
            return None
        r, _, _ = select.select([sys.stdin], [], [], timeout)
        if not r:
            return None
        data = os.read(sys.stdin.fileno(), 64)
        return data.decode('utf-8', 'replace') or None

    def read_until(self, pattern, timeout):
        buf = b''
        end = time.monotonic() + timeout
        fd = sys.stdin.fileno()
        while time.monotonic() < end:
            r, _, _ = select.select([fd], [], [], max(0.0, end - time.monotonic()))
            if not r:
                break
            buf += os.read(fd, 1024)
            if re.search(pattern, buf):
                break
        return buf


def probe(term, caps):
    """One batched query, DA1 last as the sentinel (spec §9.5). Skips DECRQM on Apple Terminal."""
    apple = os.environ.get('TERM_PROGRAM') == 'Apple_Terminal'
    q = '\x1b]10;?\x1b\\\x1b]11;?\x1b\\'
    if not apple:
        q += '\x1b[?2026$p'
    q += '\x1b[c'
    term.write(q)
    term.flush()
    buf = term.read_until(rb'\x1b\[\?[0-9;]*c', 0.35)
    colours = {}
    for which, r, g, b in re.findall(rb'\x1b\]1([01]);rgb:([0-9a-fA-F]+)/([0-9a-fA-F]+)/([0-9a-fA-F]+)', buf):
        def chan(x):
            return int(x, 16) * 255 // (16 ** len(x) - 1)
        colours[which] = to_hex((chan(r), chan(g), chan(b)))
    caps.fg, caps.bg = colours.get(b'0'), colours.get(b'1')
    m = re.search(rb'\x1b\[\?2026;(\d)\$y', buf)
    if apple:
        caps.sync_reply = 'skipped (Apple Terminal prints the query)'
    elif m:
        caps.sync_reply = {b'0': 'unknown mode', b'1': 'supported', b'2': 'supported', b'3': 'always on',
                           b'4': 'not supported'}.get(m.group(1), 'odd reply')
        caps.sync = m.group(1) in (b'1', b'2')
    else:
        caps.sync_reply = 'no reply'


def detect(args):
    caps = Caps()
    caps.tty = sys.stdout.isatty() and os.environ.get('TERM') != 'dumb'
    caps.keys = caps.tty and sys.stdin.isatty() and termios is not None
    caps.width = shutil.get_terminal_size((100, 30)).columns
    if caps.width < 20:
        caps.width = 100
    env_colour = os.environ.get('WISP_COLOR', '')
    if args.colors != 'auto':
        caps.depth = {'truecolor': 'truecolor', '256': '256', '16': '16', 'none': 'none'}[args.colors]
    elif os.environ.get('NO_COLOR'):
        caps.depth = 'none'
    elif env_colour in ('truecolor', '256', '16', 'none'):
        caps.depth = env_colour
    elif not caps.tty and not os.environ.get('FORCE_COLOR'):
        caps.depth = 'none'
    elif os.environ.get('COLORTERM', '').lower() in ('truecolor', '24bit') or os.environ.get('WT_SESSION'):
        caps.depth = 'truecolor'
    elif '256' in os.environ.get('TERM', ''):
        caps.depth = '256'
    else:
        caps.depth = '16'
    encoding = (getattr(sys.stdout, 'encoding', None) or '').lower().replace('-', '').replace('_', '')
    # ASCII when asked, on a dumb terminal, or when the output can't carry UTF-8 (spec §9.2)
    caps.ascii = args.ascii or os.environ.get('TERM') == 'dumb' or encoding not in ('utf8', 'utf8sig')
    if encoding not in ('utf8', 'utf8sig'):
        try:
            sys.stdout.reconfigure(errors='replace')   # a last line of defence for stray characters
        except (AttributeError, ValueError):
            pass
    caps.amb_wide = args.ambiguous_wide
    motion = args.motion or os.environ.get('WISP_MOTION', 'full')
    caps.motion = 'reduced' if args.reduced_motion else (motion if motion in ('full', 'calm', 'reduced') else 'full')
    return caps


def pick_theme(args, caps):
    want = args.theme if args.theme != 'auto' else os.environ.get('WISP_THEME', 'auto')
    if want in ('dark', 'light'):
        return want
    if caps.bg:
        return 'dark' if oklab(caps.bg)[0] < 0.6 else 'light'
    fgbg = os.environ.get('COLORFGBG', '')
    if fgbg:
        try:
            idx = int(fgbg.split(';')[-1])
            return 'light' if idx in (7, 9, 10, 11, 12, 13, 14, 15) else 'dark'
        except ValueError:
            pass
    return 'dark'


# ════════════════════════════════════════════ painting ════════════════════════════════════════════

def cell_width(ch):
    if unicodedata.combining(ch):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ('W', 'F') else 1


def text_width(s):
    return sum(cell_width(c) for c in s)


class Paint:
    """Token -> SGR by colour depth. A colour is a token name, or (hex, token) for animated in-between colours."""

    def __init__(self, caps, tok):
        self.caps = caps
        self.tok = tok
        self.theme = caps.theme

    def _code(self, colour, ground):
        depth = self.caps.depth
        if colour is None or depth == 'none':
            return None
        if isinstance(colour, tuple):
            hexv, name = colour
        else:
            name = ALIASES.get(colour, colour)
            hexv = self.tok[name]
        name = ALIASES.get(name, name)
        if depth == 'truecolor':
            r, g, b = rgb(hexv)
            return '%d;2;%d;%d;%d' % (38 if ground == 'fg' else 48, r, g, b)
        if depth == '256':
            idx = ANSI256[self.theme].get(name) if hexv == self.tok.get(name) else None
            idx = idx if idx is not None else nearest256(hexv)
            return '%d;5;%d' % (38 if ground == 'fg' else 48, idx)
        # ANSI-16: brightness changes don't exist here, so in-between colours fall back to their token
        if ground == 'bg':
            return '43' if name == 'warn.fill' else None
        if name == 'warn.on':
            return '30'
        code = ANSI16[self.theme].get(name)
        return None if code is None else str(code)

    def sgr(self, st):
        parts = []
        for flag, code in (('b', '1'), ('i', '3'), ('u', '4'), ('r', '7'), ('s', '9')):
            if st.get(flag):
                parts.append(code)
        for ground in ('fg', 'bg'):
            c = self._code(st.get(ground), ground)
            if c:
                parts.append(c)
        if self.caps.depth == 'none' and st.get('chip'):
            parts.append('7')   # NO_COLOR: chips become reverse video
        return '\x1b[%sm' % ';'.join(parts) if parts else ''

    def line(self, segs, width=None, fill_bg=None):
        """Render [(text, style), ...]; truncate to width; optionally paint a background to the width."""
        width = width or self.caps.width - 1
        out, used = [], 0
        segs = [(t, s) for t, s in segs if t]
        while segs and not fill_bg and not (segs[-1][1] or {}).get('bg') and segs[-1][0].endswith(' '):
            segs[-1] = (segs[-1][0].rstrip(' '), segs[-1][1])   # copy-clean: no trailing spaces (spec §4.4)
            if not segs[-1][0]:
                segs.pop()
        for text, st in segs:
            st = st or {}
            if fill_bg and 'bg' not in st:
                st = dict(st, bg=fill_bg)
            chunk = ''
            for ch in text:
                w = cell_width(ch)
                if used + w > width:
                    break
                chunk += ch
                used += w
            if chunk:
                code = self.sgr(st)
                out.append(code + chunk + ('\x1b[0m' if code else ''))
            if used >= width:
                break
        if fill_bg and used < width and self.caps.depth != 'none':
            code = self.sgr({'bg': fill_bg})
            if code:
                out.append(code + ' ' * (width - used) + '\x1b[0m')
        return ''.join(out)


def S(fg=None, **kw):
    kw['fg'] = fg
    return kw


def wrap(text, width, indent=3):
    """Greedy word wrap for captions; continuation lines keep the caption's indent."""
    lead = len(text) - len(text.lstrip(' '))
    words, lines, cur = text.split(), [], ''
    for w in words:
        if cur and lead + len(cur) + 1 + len(w) > width:
            lines.append(cur)
            cur = w
        else:
            cur = (cur + ' ' + w) if cur else w
    lines.append(cur)
    return [' ' * (lead if i == 0 else max(lead, indent)) + ln for i, ln in enumerate(lines)]


# ════════════════════════════════════════════ the presence orb (spec §6.3) ════════════════════════════════════════════

class Orb:
    """The dock orb as a pure function of state and time. Returns three (char, colour) cells."""

    def __init__(self, tok, caps):
        self.tok = tok
        self.caps = caps
        self.k = 2.0 if caps.motion == 'calm' else 1.0       # Calm: loops twice as slow...
        self.amp = 0.5 if caps.motion == 'calm' else 1.0     # ...and half as wide
        self.reduced = caps.motion == 'reduced'

    def c(self, name):
        return (self.tok[ALIASES.get(name, name)], name)

    def blend(self, a, b, t, name):
        """t of token a over token b, tagged with the token to use where blending is impossible."""
        return (mix(self.tok[ALIASES.get(a, a)], self.tok[ALIASES.get(b, b)], t), name)

    def cells(self, on, ring, core, core_char=CORE):
        if self.caps.ascii:
            return [('(' if on else ' ', ring), (core_char if core_char != CORE else '*', core),
                    (')' if on else ' ', ring)]
        left, right = ring_cells(on)
        return [(left, ring), (core_char, core), (right, ring)]

    def frame(self, state, t, chunk_t=None, progress=None):
        k, amp = self.k, self.amp
        if state == 'rest':
            return self.cells(REST, self.c('orb.ring'), self.c('accent'))
        if state == 'listening':
            return self.cells(ALL, self.c('orb.ring.listen'), self.c('orb.core.dim'))
        if state == 'thinking':
            step = 0 if self.reduced else int(t // (GRID['Thinking'] * k)) % 4
            on = ALL - {step, step + 4}
            if self.reduced:
                core = self.c('orb.core.dim')
            else:
                u = q(t, GRID['Brightness']) % (1600 * k)
                rise = 400 * k
                s = EASE_IN_OUT(u / rise) if u < rise else (1 - EASE_IN_OUT((u - rise) / rise) if u < 2 * rise else 0)
                core = self.blend('accent', 'orb.core.dim', s * amp, 'accent')
            core_char = '-\\|/'[step] if self.caps.ascii else CORE
            return self.cells(on, self.c('text'), core, core_char)
        if state == 'answering':
            if self.reduced:
                s = 1.0
            else:
                since = q(t - (chunk_t if chunk_t is not None else -10 ** 9), GRID['Brightness'])
                if since <= 400:
                    s = 1 - EASE_OUT(since / 400)
                elif since < 600:
                    s = 0.0
                else:
                    s = wave(since - 600, 4000 * k) * amp
            core = self.blend('accent', 'orb.core.dim', s, 'accent')
            return self.cells(REST, self.c('orb.ring.speak'), core)
        if state == 'working':
            if progress is not None:
                n = max(1, math.ceil(progress * 8))
                return self.cells(set(range(n)), self.c('text'), self.c('accent'))
            p = 0.0 if self.reduced else wave(q(t, GRID['Brightness']), 2000 * k) * amp
            return self.cells(REST, self.blend('text', 'orb.ring', p, 'orb.ring'), self.c('accent'))
        if state == 'needs':
            repeats = 0 if self.reduced else (1 if self.caps.motion == 'calm' else 3)
            op = keyframes(q(t % 1600, GRID['Needs-you']), BEAT_KEYS, BEAT) if t < 1600 * repeats else 1.0
            col = self.blend('warn.glyph', 'bg', op, 'warn.glyph')
            core_char = '!' if self.caps.ascii else CORE
            return self.cells(ALL, col, col, core_char)
        if state == 'done':
            total = 1200 if self.reduced else 2200 * k
            if t >= total:
                return self.frame('rest', 0)
            if self.caps.ascii:
                return [('(', self.c('ok')), ('+', self.c('ok')), (')', self.c('ok'))]
            out = []
            for i, ch in enumerate(CHECK):
                if not self.reduced and t < 120:
                    col = self.blend('ok', 'bg', 0.5, 'ok')
                elif not self.reduced and (400 + 200 * i) * k <= t < (600 + 200 * i) * k:
                    col = self.blend('ok', 'text', 0.45, 'ok')      # the glint
                else:
                    col = self.c('ok')
                out.append((ch, col))
            return out
        if state == 'offline':
            op = 1.0 if self.reduced else 1 - 0.5 * wave(q(t, GRID['Offline']), 5000 * k) * amp
            col = self.blend('orb.off', 'bg', op, 'orb.off')
            return self.cells(REST, col, col)
        if state == 'stopped':
            if t < 300 * k and not self.reduced:
                return self.cells(REST, self.c('orb.ring'), self.c('orb.core.dim'))
            return self.frame('rest', 0)
        raise ValueError(state)


def comet(caps, t, reduced):
    if reduced:
        return ASCII['running'] if caps.ascii else UNICODE['running']
    k = 2.0 if caps.motion == 'calm' else 1.0
    if caps.ascii:
        return COMET_ASCII[int(t // (GRID['Comet'] * k)) % 4]
    return COMET[int(t // (GRID['Comet'] * k)) % 8]


# ════════════════════════════════════════════ the animation runner (spec §6.1, §9.3) ════════════════════════════════════════════

class Skip(Exception):
    pass


class Quit(Exception):
    pass


class Runner:
    def __init__(self, term, caps, paint):
        self.term, self.caps, self.p = term, caps, paint

    def play(self, render, duration_ms, lines, caption=None, static_at=None):
        """Draw render(t) -> [line strings] in place, writing only frames that changed. Any key skips, q quits."""
        caps = self.caps
        if not caps.tty or caps.motion == 'reduced':
            t = duration_ms if static_at is None else static_at
            for ln in render(t):
                self.term.write(ln + '\n')
            if caption:
                for ln in wrap(caption, caps.width - 2):
                    self.term.write(self.p.line([(ln, S('text.faint'))]) + '\n')
            self.term.flush()
            if caps.tty:
                self.wait(1500)
            return
        self.term.write('\n' * lines)
        start = time.monotonic()
        last = None
        written = 0
        try:
            while True:
                t = (time.monotonic() - start) * 1000
                if t > duration_ms:
                    t = duration_ms
                frame = render(t)
                if frame != last:
                    out = '\x1b[%dA' % lines
                    for ln in frame[:lines]:
                        out += '\r\x1b[2K' + ln + '\n'
                    for _ in range(lines - len(frame)):
                        out += '\r\x1b[2K\n'
                    self.term.frame(out)
                    written += 1
                    last = frame
                if t >= duration_ms:
                    break
                key = self.term.key(0.02)
                if key:
                    if key.lower().startswith('q'):
                        raise Quit()
                    raise Skip()
        except Skip:
            pass
        secs = (time.monotonic() - start)
        if caption:
            note = '%s (%d frames written in %.1f s: %.1f a second.)' % (caption, written, secs,
                                                                     written / max(secs, 0.001))
            for ln in wrap(note, caps.width - 2):
                self.term.write(self.p.line([(ln, S('text.faint'))]) + '\n')
            self.term.flush()

    def wait(self, ms):
        key = self.term.key(ms / 1000)
        if key and key.lower().startswith('q'):
            raise Quit()


# ════════════════════════════════════════════ sections ════════════════════════════════════════════

class Preview:
    def __init__(self, term, caps, tok, corrected):
        self.term, self.caps, self.tok, self.corrected = term, caps, tok, corrected
        self.p = Paint(caps, tok)
        self.g = ASCII if caps.ascii else (dict(UNICODE, **NARROW) if caps.amb_wide else UNICODE)
        self.orb = Orb(tok, caps)
        self.run = Runner(term, caps, self.p)
        self.W = max(40, min(caps.width - 1, 100))

    # helpers
    def out(self, segs=(), **kw):
        self.term.write(self.p.line(list(segs), **kw) + '\n')

    def blank(self):
        self.term.write('\n')

    def note(self, text, style='text.faint'):
        for ln in wrap(text, self.caps.width - 2):
            self.out([(ln, S(style))])

    def title(self, first, second):
        """The brand's two-halves heading: a light lead, a heavier second half (spec §4.2)."""
        self.blank()
        self.out([(' ' + first + ' ', S('text.muted')), (second, S('text', b=True))])
        self.blank()

    def orb_segs(self, cells):
        return [(ch, S(col)) for ch, col in cells]

    def sep(self):
        return ' %s ' % self.g['separator']

    # ── intro ──
    def intro(self):
        c = self.caps
        logo = self.logo_segs()
        self.blank()
        self.out([(' ', None)] + logo + [('  brand preview', S('text.muted'))])
        bg = c.bg or 'unknown'
        facts = [
            ('colours', c.depth + (' (NO_COLOR)' if os.environ.get('NO_COLOR') and c.depth == 'none' else '')),
            ('theme', '%s, background %s%s' % (c.theme, bg, ', tokens corrected for it' if self.corrected else '')),
            ('synchronized output', c.sync_reply),
            ('motion', c.motion),
            ('glyphs', 'ASCII' if c.ascii else 'Unicode, ambiguous-wide safe' if c.amb_wide else 'Unicode'),
        ]
        for k, v in facts:
            self.out([('   %-20s' % k, S('text.faint')), (v, S('text.muted'))])
        if c.tty:
            self.out([('   any key skips an animation, q quits', S('text.faint'))])

    def logo_segs(self):
        """The static logo: ink ring, wisp-blue core, the wordmark in bold (spec §3.1)."""
        if self.caps.ascii:
            return [('(', S('text')), ('*', S('accent')), (')', S('text')), (' wisp', S('text', b=True))]
        left, right = ring_cells(REST)
        return [(left, S('text')), (CORE, S('accent')), (right, S('text')), (' wisp', S('text', b=True))]

    # ── palette ──
    def palette(self):
        self.title('Colour:', 'Dark+ and Stone, in the terminal')
        rows = [
            ('Text', ['text', 'text.muted', 'text.faint', 'thinking', 'user']),
            ('Meaning', ['accent', 'ok', 'warn', 'warn.glyph', 'error']),
            ('Roles', ['role.coordinator', 'role.coder', 'role.reviewer', 'syn.number']),
            ('Orb', ['orb.ring', 'orb.ring.listen', 'orb.ring.speak', 'orb.core.dim', 'orb.off']),
        ]
        bg = self.tok['bg']
        for label, names in rows:
            self.out([('   %-8s' % label, S('text.faint'))])
            for name in names:
                hexv = self.tok[name]
                ratio = contrast(hexv, bg)
                need = MIN_CONTRAST.get(name)
                mark = '' if need is None else ('  >= %.1f' % need if ratio >= need else '  BELOW %.1f' % need)
                swatch = ('      ', S(None, bg=name)) if self.caps.depth != 'none' else ('[%s]' % name[:4], {'r': 1})
                self.out([('     ', None), swatch, ('  ', None), ('%-17s' % name, S(name)),
                          ('%s  %5.2f:1%s' % (hexv, ratio, mark), S('text.muted'))])
        self.out([('   %-8s' % 'Surfaces', S('text.faint'))])
        for name in ['surface', 'sunken', 'line', 'accent.tint', 'warn.tint', 'diff.add', 'diff.del']:
            hexv = self.tok[name]
            sample = ' text on it %5.2f:1 ' % contrast(self.tok['text'], hexv)
            self.out([('     ', None), (sample, S('text', bg=name)), ('  %-14s %s' % (name, hexv), S('text.muted'))])
        self.out([('     ', None), (' Needs you ', S('warn.on', bg='warn.fill', b=True, chip=True)),
                  ('  the chip: warn.on on warn.fill, %.1f:1' % contrast(self.tok['warn.on'], self.tok['warn.fill']),
                   S('text.muted'))])
        if self.caps.depth == '16':
            self.note('   ANSI-16: tones collapse to the terminal\'s own colours, so the swatches above are '
                      'approximate.')
        if self.caps.depth == 'none':
            self.note('   NO_COLOR: no colour at all. Bold, italic, underline and reverse carry the meaning.')

    # ── wordmark ──
    def wordmark(self):
        self.title('The mark:', 'Halo, three cells wide')
        sep = self.sep()
        self.out([('   ', None)] + self.logo_segs() + [('   the logo: static, in the header and the banner',
                                                         S('text.faint'))])
        self.out([('   ', None)] + self.orb_segs(self.orb.frame('rest', 0)) +
                 [('        the orb at rest: live, by the composer', S('text.faint'))])
        a = [('(', S('text')), ('*', S('accent')), (')', S('text')), (' wisp', S('text', b=True))]
        self.out([('   ', None)] + a + [('   ASCII', S('text.faint'))])
        self.blank()
        self.out([('   banner  ', S('text.faint'))] + self.logo_segs() +
                 [(' 0.1.0' + sep + 'Jarvis' + sep + '~/Projects/Wisp', S('text.muted'))])
        self.blank()
        for i, row in enumerate(self.splash_rows(None)):
            self.out([('   ', None)] + row)
        if not self.caps.ascii:
            self.blank()
            for i, ln in enumerate(SPLASH_ASCII):
                self.out([('   ', None), (ln, S('orb.ring')), ('   ', None)] + self.splash_text(i, ascii_=True))

    def splash_text(self, row, ascii_=False):
        sep = ' - ' if ascii_ else self.sep()
        return [
            [('wisp', S('text', b=True))],
            [('Your own AI. ', S('text.muted')), ('With a computer of its own.', S('text', b=True))],
            [('0.1.0' + sep + 'Jarvis' + sep + '~/Projects/Wisp', S('text.muted'))],
        ][row]

    def splash_rows(self, t, reveal=False):
        """The splash orb at time t (None = rest). Returns three rows of segments."""
        if self.caps.ascii:
            return [[(SPLASH_ASCII[r], S('orb.ring')), ('   ', None)] + self.splash_text(r, ascii_=True)
                    for r in range(3)]
        tok = self.tok
        order = [4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 0, 1]   # clockwise from the gap's end
        # Reveal: two lights per 40 ms step, clockwise from the gap's end (8 steps, 320 ms); the core fades in
        shown = set(order) if not reveal or t is None else set(order[:2 * (int(t // 40) + 1)])
        core_op = 1.0 if not reveal or t is None else min(1.0, (int(t // 40) + 1) / 8)
        rows = []
        for r in range(3):
            row = []
            for col in range(8):
                if (col, r) in SPLASH_CORE:
                    row.append((SPLASH_CORE[(col, r)], S((mix(tok['accent'], tok['bg'], core_op), 'accent'))))
                    continue
                bits = 0
                for light in SPLASH_CELLS.get((col, r), ()):
                    if light not in shown:
                        continue
                    _, _, bit = SPLASH_LIGHT_BITS[light]
                    bits |= bit
                if bits:
                    row.append((chr(0x2800 + bits), S('orb.ring')))
                else:
                    row.append((' ', None))
            row.append(('   ', None))
            row.extend(self.splash_text(r))
            rows.append(row)
        return rows

    # ── motion: startup reveal ──
    def reveal(self):
        self.title('Motion:', 'the startup reveal')
        sep = self.sep()
        order = [2, 3, 4, 5, 6, 7, 0]     # the ring assembles clockwise from the gap's end, 40 ms per light

        def render(t):
            t = q(t % 1700, GRID['Startup'])                    # the reveal's 40 ms grid
            n = len(order) if self.caps.motion == 'reduced' else min(len(order), int(t // GRID['Startup']))
            on = set(order[:n])
            core_op = 1.0 if self.caps.motion == 'reduced' else min(1.0, 0.45 + 0.55 * t / 280)
            core = (mix(self.tok['accent'], self.tok['orb.core.dim'], (core_op - 0.45) / 0.55), 'accent')
            if self.caps.ascii:
                cells = [('(' if n >= 4 else ' ', 'text'), ('*', core), (')' if n >= 1 else ' ', 'text')]
            else:
                left, right = ring_cells(on)
                cells = [(left, 'text'), (CORE, core), (right, 'text')]
            segs = [(' ', None)] + [(ch, S(col)) for ch, col in cells]
            segs += [(' wisp', S('text', b=True)), (' 0.1.0' + sep + 'Jarvis' + sep + '~/Projects/Wisp',
                                                    S('text.muted'))]
            return [self.p.line(segs)]
        self.run.play(render, 3400, 1, '   280 ms: the core first, then seven ring lights clockwise, 40 ms apart. '
                                       'Never blocks typing.', static_at=1000)
        self.blank()
        self.note('   The splash reveals its ring and core, then stays still.')

        def splash(t):
            rows = self.splash_rows(t, reveal=True) if t < 320 else self.splash_rows(None)
            return [self.p.line([(' ', None)] + row) for row in rows]
        self.run.play(splash, 1000, 3, '   The final frame holds without idle motion.')

    # ── motion: the orb's states ──
    def states(self):
        self.title('Motion:', 'the presence orb, state by state')
        q = 0.5 if self.caps.quick else 1.0
        plan = [('rest', 1800, 'Ready', 'Idle: the orb stays still.'),
                ('listening', 1800, 'Listening', 'The composer has text: the ring closes around you.'),
                ('thinking', 4000, 'Thinking', 'Two arcs turn, one step per 500 ms; the core sparkles every 1.6 s.'),
                ('answering', 4500, 'Answering', 'Each chunk lights the core; a 4 s breath fills the pauses.'),
                ('working', 4000, 'Working', 'A steady 2 s pulse in the ring.'),
                ('done', 2400, 'Done', 'The ring redraws itself as a check; a glint runs along it.'),
                ('needs', 5200, 'Needs you', 'Gold, gathered, the 1.6 s beat three times, then still.'),
                ('offline', 5000, 'Offline. Reconnecting', 'Grey, with a slow dim breath every 5 s.'),
                ('rest', 1800, 'Ready', 'Back to idle: the orb stays still.')]
        plan = [(s, d * q if s not in ('needs', 'done') else d, l, n) for s, d, l, n in plan]
        starts, acc = [], 0
        for s, d, l, n in plan:
            starts.append(acc)
            acc += d
        total = acc
        chunks = [0, 300, 700, 800, 1300, 2600]   # token arrivals during "answering", then a pause
        prev = {'cells': None}

        def render(t):
            i = max(j for j in range(len(plan)) if starts[j] <= t) if t < total else len(plan) - 1
            state, _, label, note = plan[i]
            ts = t - starts[i]
            chunk_t = max([c for c in chunks if c <= ts], default=None) if state == 'answering' else None
            cells = self.orb.frame(state, ts, chunk_t=chunk_t)
            # The state morph: shapes switch at once; colours blend over 240 ms in three 80 ms steps.
            if i > 0 and ts < 240 and self.caps.motion != 'reduced':
                pstate, pd = plan[i - 1][0], plan[i - 1][1]
                old = self.orb.frame(pstate, pd)
                u = MORPH(min(1.0, (int(ts // 80) + 1) * 80 / 240))
                cells = [(ch, (mix(col[0], o[1][0], u), col[1])) for (ch, col), o in zip(cells, old)]
            lab_style = S('warn', b=True) if state == 'needs' else S('ok') if state == 'done' else S('text.muted')
            segs = [(' ', None)] + self.orb_segs(cells) + [('  ', None), (label, lab_style)]
            return [self.p.line(segs), self.p.line([('      ' + note, S('text.faint'))])]
        self.run.play(render, total, 2, '   One element moves at a time while working; idle stays still.',
                      static_at=None)
        if not self.caps.tty or self.caps.motion == 'reduced':
            self.static_orb_table()

    def static_orb_table(self):
        self.out([('   Every state, as reduced motion shows it:', S('text.faint'))])
        for state, label in [('rest', 'Ready'), ('listening', 'Listening'), ('thinking', 'Thinking'),
                             ('answering', 'Answering'), ('working', 'Working'), ('done', 'Done'),
                             ('needs', 'Needs you'), ('offline', 'Offline'), ('stopped', 'Stopped')]:
            t = 10 ** 6 if state in ('needs',) else (300 if state == 'done' else 0)
            self.out([('     ', None)] + self.orb_segs(self.orb.frame(state, t)) + [('  ' + label, S('text.muted'))])
        if not self.caps.ascii:
            frames = ' '.join(''.join(ring_cells(ALL - {s, s + 4})[0] + CORE + ring_cells(ALL - {s, s + 4})[1])
                              for s in range(4))
            self.out([('     thinking frames, 500 ms each: ', S('text.faint')), (frames, S('text'))])

    # ── motion: waiting for the first token, then streaming ──
    def first_token(self):
        self.title('Motion:', 'waiting for the first token, then the answer')
        sep = self.sep()
        answer = ('Misses now say which part of the prefix changed: the system prompt, the tools or the '
                  'messages. The prefix-stability test covers each case.')
        cap = min(self.W - 4, 76)
        words, lines_, cur = answer.split(' '), [], ''
        for w in words:
            if len(cur) + len(w) + 1 > cap:
                lines_.append(cur)
                cur = w
            else:
                cur = (cur + ' ' + w).strip()
        lines_.append(cur)
        first = 2600                        # the first token arrives here
        cps = 70.0                          # characters per second while streaming
        chunk_every = 90
        end_stream = first + len(answer) / cps * 1000
        done_at = end_stream + 700

        def render(t):
            if t < first:
                state, ts = 'thinking', t
            elif t < done_at:
                state, ts = 'answering', t - first
            else:
                state, ts = 'done', t - done_at
            shown = 0 if t < first else int(min(len(answer), (t - first) / 1000 * cps) // 6 * 6)
            last_chunk = None
            if first <= t < end_stream:
                last_chunk = (t - first) // chunk_every * chunk_every
            elif t >= end_stream:
                last_chunk = end_stream - first
            cells = self.orb.frame(state, ts, chunk_t=last_chunk)
            # The status sentence: nothing for 300 ms, then "Thinking", then a timer from 2 s.
            if state == 'thinking':
                status = [] if t < 300 else [('Thinking', S('thinking'))]
                if t >= 2000:
                    status.append((sep + '0:%02d' % int(t // 1000), S('text.muted')))
            elif state == 'answering':
                status = [('Answering', S('text.muted')), (sep + 'esc to stop', S('text.faint'))]
            else:
                status = [('Done', S('ok')), (sep + '4 s' + sep + '$0.004' + sep + 'cache 92%', S('text.muted'))]
            body, left = [], shown
            for i, ln in enumerate(lines_):
                if left <= 0:
                    break
                part = ln[:left]
                left -= len(ln) + 1
                marker = (self.g['wisp'] + ' ', S('accent')) if i == 0 else ('  ', None)
                segs = [(' ', None), marker, (part, S('text'))]
                caret_here = left <= 0 or i == len(lines_) - 1
                if caret_here and state == 'answering':
                    core_col = cells[1][1]
                    segs.append(((' ' if not part.endswith(' ') else '') + self.g['caret'], S(core_col)))
                body.append(self.p.line(segs))
            while len(body) < len(lines_):
                body.append('')
            return [self.p.line([(' ', None), (self.g['user'] + ' ', S('user')),
                                 ('Make cache misses name their cause', S('text'))], fill_bg='surface')] + [''] + \
                body + [''] + [self.p.line([(' ', None)] + self.orb_segs(cells) + [('  ', None)] + status)]
        n = 1 + 1 + len(lines_) + 1 + 1
        self.run.play(render, done_at + 2600, n, '   The caret is the core: it breathes with the orb and never '
                                                 'reaches scrollback.', static_at=first + 1500)

    # ── motion: tool rows ──
    def tools(self):
        self.title('Motion:', 'a tool runs, then succeeds, fails or stops')
        sep = self.sep()
        q = 0.6 if self.caps.quick else 1.0
        rows = [('Ran', 'Running', 'cargo test -p wisp-core', 'ok', '41 passed' + sep + '0:%02d', 2400 * q),
                ('Ran', 'Running', 'cargo clippy --workspace', 'fail', '2 errors' + sep + '0:%02d', 2000 * q),
                ('Ran', 'Running', 'cargo build --release', 'stop', 'Stopped' + sep + '0:%02d', 1600 * q)]
        starts, acc = [], 300
        for r in rows:
            starts.append(acc)
            acc += r[5] + 400
        total = acc + 800
        verb_w = 9

        def render(t):
            lines = []
            any_running = False
            for (done_v, run_v, target, outcome, meta, dur), st in zip(rows, starts):
                if t < st:
                    lines.append('')
                    continue
                el = t - st
                if el < dur:
                    any_running = True
                    glyph = (comet(self.caps, el, self.caps.motion == 'reduced'), S('accent'))
                    verb, right = run_v, '0:%02d' % int(el // 1000)
                    rstyle = S('text.muted')
                else:
                    since = el - dur
                    secs = int(dur // 1000)
                    if outcome == 'ok':
                        col = (mix(self.tok['ok'], self.tok['text'], 0.5), 'ok') if since < 120 and \
                            self.caps.motion != 'reduced' else 'ok'
                        glyph, verb, right = (self.g['done'], S(col)), done_v, meta % secs
                        rstyle = S('text.muted')
                    elif outcome == 'fail':
                        glyph, verb, right = (self.g['failed'], S('error')), done_v, meta % secs
                        rstyle = S('error')
                    else:
                        glyph, verb, right = (self.g['stopped'], S('text.muted')), 'Stopped', meta % secs
                        rstyle = S('text.muted')
                left = [(' ', None), glyph, (' ', None), (verb.ljust(verb_w), S('text', b=True)),
                        (target, S('text'))]
                pad = self.W - 2 - text_width(''.join(x for x, _ in left)) - text_width(right)
                lines.append(self.p.line(left + [(' ' * max(1, pad), None), (right, rstyle)]))
            state = 'working' if any_running else 'rest'
            status = [('Working' + sep + 'cargo', S('text.muted'))] if any_running else [('Ready', S('text.faint'))]
            lines += ['', self.p.line([(' ', None)] + self.orb_segs(self.orb.frame(state, t)) + [('  ', None)] +
                                      status)]
            return lines
        self.run.play(render, total, 5, '   Comet: 8 frames, 150 ms each. Done glints for 120 ms; failure and '
                                        'stop just settle.')

    # ── motion: notifications ──
    def notify(self):
        self.title('Motion:', 'a proactive message, then one that needs you')
        sep = self.sep()
        info_at, needs_at = 400, 3600
        total = needs_at + 1600 * 3 + 1000

        def render(t):
            lines = []
            if t >= info_at:
                el = q(t - info_at, GRID['Notification'])
                if self.caps.motion == 'reduced':
                    col = 'accent'
                elif el < 200:
                    col = (mix(mix(self.tok['accent'], self.tok['text'], 0.5), self.tok['accent'],
                               EASE_IN_OUT(el / 200)), 'accent')
                elif el < 800:
                    col = (mix(self.tok['accent'], mix(self.tok['accent'], self.tok['text'], 0.5),
                               EASE_OUT((el - 200) / 600)), 'accent')
                else:
                    col = 'accent'
                lines += [self.p.line([(' ', None), (self.g['wisp'] + ' ', S(col)), ('Jarvis', S('text', b=True)),
                                       (sep + 'routine' + sep + '7:30 AM', S('text.muted'))]),
                          self.p.line([('   The price check ran. Bloom Mini is down $12, to $349.', S('text'))]),
                          self.p.line([('   ctrl-o show details', S('text.faint'))]), '']
            else:
                lines += ['', '', '', '']
            orb_state, ots = 'rest', t
            info_core = None
            if info_at <= t < needs_at and isinstance(col, tuple):
                info_core = col                              # the dock orb's core swells with the notice
            if t >= needs_at:
                el = t - needs_at
                cells = self.orb.frame('needs', el)
                g = (self.g['needs_you'] + ' ', S(cells[1][1]))
                lines += [self.p.line([(' ', None), g, ('Jarvis needs you', S('warn', b=True)),
                                       (sep + 'follow-up', S('text.muted'))]),
                          self.p.line([('   Send this morning\'s summary to home.example? It\'s ready.', S('text'))]),
                          self.p.line([('   ', None), (' ctrl-y ', S('warn.on', bg='warn.fill', b=True, chip=True)),
                                       (' review' + sep + 'ctrl-n not now', S('text.muted'))])]
                orb_state, ots = 'needs', el
            else:
                lines += ['', '', '']
            badge = [('1 needs you', S('warn', b=True))] if t >= needs_at else [('Ready', S('text.faint'))]
            cells = self.orb.frame(orb_state, ots)
            if info_core and orb_state == 'rest':
                cells = [cells[0], (cells[1][0], info_core), cells[2]]
            lines += ['', self.p.line([(' ', None)] + self.orb_segs(cells) + [('  ', None)] + badge)]
            return lines
        self.run.play(render, total, 9, '   Informational: one 800 ms swell. Needs you: the beat, three times, '
                                        'then still. Unfocused: OSC 9 / BEL.')

    # ── motion: the agent tree ──
    def tree(self):
        self.title('Motion:', 'the live tree of projects and agents')
        g, sep = self.g, self.sep()
        P = {'pipe': g['pipe'], 'tee': g['tee'], 'elbow': g['elbow']}
        rows = [  # (indent prefix parts, glyph kind, name, role token, role, activity, time, cost)
            ([], 'work', 'Jarvis', 'accent', '', 'two projects', '', '$0.42 today'),
            ([P['tee']], 'work', 'cache-fingerprints', 'role.coordinator', 'project', 'drafting the plan',
             '2:14', '$0.31'),
            ([P['pipe'], P['tee']], 'work', 'researcher', 'text', '', 'reading prompt-caching.md',
             '0:41', '$0.04'),
            ([P['pipe'], P['tee']], 'needs', 'coder', 'role.coder', '', 'needs you: run cargo test', '',
             '$0.12'),
            ([P['pipe'], P['elbow']], 'wait', 'reviewer', 'role.reviewer', '', 'waiting for coder', '',
             ''),
            ([P['elbow']], 'done', 'tui-research', 'role.coordinator', 'project', 'report ready', '',
             '$1.20'),
        ]
        # The bus runs top to bottom through the coordinator: siblings never message each other (architecture D3).
        msgs = [(1200, 1, 3, 'coordinator ' + g['arrow'] + ' coder', '"tests first, then the patch"'),
                (3600, 3, 1, 'coder ' + g['arrow'] + ' coordinator', '"patch ready, 3 files"'),
                (5600, 1, 4, 'coordinator ' + g['arrow'] + ' reviewer', '"please review the patch"')]
        k = 2.0 if self.caps.motion == 'calm' else 1.0
        total = 8800

        def glyph_for(kind, role, t, needs_t):
            if kind == 'work':
                if self.caps.motion == 'reduced':
                    return g['wisp'], S(role)
                s = wave(q(t, GRID['Brightness']), 2000 * k) * (0.5 if self.caps.motion == 'calm' else 1.0)
                return g['wisp'], S((mix(self.tok[ALIASES.get(role, role)], self.tok['bg'], 1 - 0.4 * s), role))
            if kind == 'needs':
                return g['needs_you'], S(self.orb.frame('needs', needs_t)[1][1])
            if kind == 'wait':
                return g['waiting'], S('text.faint')
            return g['done'], S('ok')

        def glyph_col(r):
            return 1 + sum(text_width(p) for p in rows[r][0])

        name_end = max(glyph_col(r) + 2 + len(rows[r][2]) for r in range(len(rows))) + 2

        def path_cells(parent, child):
            """Rail cells from a parent's glyph down its children's rail to the child's glyph: (row, col)."""
            pc, cc = glyph_col(parent), glyph_col(child)
            cells = [(parent, pc)] + [(r, pc) for r in range(parent + 1, child + 1)]
            return cells + [(child, c) for c in range(pc + 1, cc + 1)]

        def render(t):
            lit, swell, note = {}, set(), None
            for at, src, dst, who, what in msgs:
                if t < at:
                    continue
                cells = path_cells(min(src, dst), max(src, dst))
                if src > dst:
                    cells.reverse()                    # an upward message travels the same rail backwards
                el = t - at
                if self.caps.motion != 'reduced':
                    head = int(el // (GRID['Message'] * k))
                    for j, cell in enumerate(cells):
                        d = head - j
                        if d == 0:
                            lit[cell] = (self.tok['accent'], 'accent')
                        elif d in (1, 2):
                            lit[cell] = (mix(self.tok['accent'], self.tok['line'], 0.5 if d == 1 else 0.25),
                                         'accent')
                    if 0 <= el - len(cells) * GRID['Message'] * k < 200:
                        swell.add(dst)
                note = (dst, who, what)                # the latest message shows under its recipient
            header = [(' Projects', S('text', b=True)),
                      ('   1 needs you', S('warn', b=True)), (sep + '3 working' + sep + '1 done', S('text.muted'))]
            out = [self.p.line(header)]
            for ri, (prefix, kind, name, role, rname, act, tm, cost) in enumerate(rows):
                segs = [(' ', None)]
                col = 1
                for part in prefix:
                    for ch in part:
                        segs.append((ch, S(lit[(ri, col)]) if (ri, col) in lit else S('line')))
                        col += 1
                gch, gst = glyph_for(kind, role, t, t)
                if (ri, col) in lit:
                    gst = S(lit[(ri, col)])
                elif ri in swell and kind != 'needs':
                    gst = S((mix(self.tok[ALIASES.get(role, role)], self.tok['text'], 0.5), role))
                segs += [(gch, gst), (' ', None), (name.ljust(name_end - col - 2), S(role, b=(ri == 0))),
                         (rname.ljust(9), S('text.faint')),
                         (act.ljust(28), S('warn') if kind == 'needs' else S('text.muted')),
                         (tm.rjust(5), S('text.muted')), (cost.rjust(12), S('text.muted'))]
                out.append(self.p.line(segs))
                if note and note[0] == ri:
                    rail = ''.join(g['pipe'] if p in (g['tee'], g['pipe']) else '  ' for p in prefix)
                    out.append(self.p.line([(' ' + rail.ljust(col + 1), S('line')), (note[1] + '  ', S('text.faint')),
                                            (note[2], S('text.muted'))]))
            while len(out) < 2 + len(rows):
                out.append('')
            return out
        self.run.play(render, total, 2 + len(rows), '   Messages travel the rail at 70 ms a cell; the recipient '
                                                   'swells for 200 ms. Working rows share one 2 s pulse.',
                      static_at=4400)

    # ── the chat turn (static) ──
    def chat(self):
        self.title('Layout:', 'one chat turn, inline')
        g, sep, W = self.g, self.sep(), self.W
        content = min(W - 4, 80)

        def row(glyph, gstyle, segs, right=None, rstyle=None, bg=None):
            left = [(' ', None), (glyph, gstyle), (' ', None)] + segs
            if right:
                pad = W - text_width(''.join(x for x, _ in left)) - text_width(right)
                left += [(' ' * max(1, pad), None), (right, rstyle or S('text.muted'))]
            self.out(left, fill_bg=bg, width=W if bg else None)

        row(g['user'], S('user'), [('Make cache misses name their cause', S('text'))], bg='surface')
        self.blank()
        row(g['rail'], S('text.faint'), [('Thinking', S('thinking')), (sep + '4 s', S('text.muted'))])
        for ln in ['Placeholder thought line one, wrapped narrowly. Placeholder',
                   'thought line two, longer than the first.']:
            row(g['rail'], S('text.faint'), [(ln, S('thinking', i=True))])
        self.blank()
        row(g['wisp'], S('accent'), [('I\'ll start with how request prefixes are built.', S('text'))])
        self.blank()
        verb = 10
        row(g['done'], S('ok'), [('Read'.ljust(verb), S('text', b=True)),
                                 ('crates/wisp-core/src/context.rs', S('text'))], '412 lines')
        row(g['done'], S('ok'), [('Searched'.ljust(verb), S('text', b=True)), ('"fingerprint"', S('text'))],
            '7 matches in 3 files')
        self.out([(' ', None), (g['done'], S('ok')), (' ', None), ('Edited'.ljust(verb), S('text', b=True)),
                  ('crates/wisp-core/src/cache.rs', S('text')),
                  (' ' * (W - 4 - verb - len('crates/wisp-core/src/cache.rs') - 7), None), ('+38', S('ok')),
                  (' %s12' % g['minus'], S('error'))])
        self.diff()
        row(g['done'], S('ok'), [('Ran'.ljust(verb), S('text', b=True)), ('cargo test -p wisp-core', S('text'))],
            '41 passed' + sep + '0:12')
        self.blank()
        para = ['Misses now say which part of the prefix changed: the system prompt,',
                'the tools or the messages. The prefix-stability test covers each case.']
        row(g['wisp'], S('accent'), [(para[0][:content], S('text'))])
        self.out([('   ', None), (para[1][:content], S('text'))])
        self.blank()
        self.out([('   18 s' + sep + '4 tools' + sep + '$0.014' + sep + 'cache 92% read', S('text.faint'))])
        self.approval()
        self.live_region()

    def diff(self):
        tok = self.tok
        W = self.W
        lines = [(' ', '209', '    fn key(req: &Request) -> Key {', None),
                 ('-', '210', '        let key = hash(&req.system);', ('hash(&req.system)',)),
                 ('+', '210', '        let key = Fingerprint::of(req)?;', ('Fingerprint::of(req)?',)),
                 (' ', '211', '        key', None)]
        for sign, num, code, words in lines:
            bg = {'-': 'diff.del', '+': 'diff.add'}.get(sign)
            segs = [('     ', None), (num.rjust(4) + ' ', S('text.muted', bg=bg)),   # faint fails 3:1 on tints
                    (sign + ' ', S({'-': 'error', '+': 'ok'}.get(sign, 'text.faint'), bg=bg))]
            rest = code
            if words:
                i = rest.index(words[0])
                segs += self.syntax(rest[:i], bg)
                wbg = 'diff.del.word' if sign == '-' else 'diff.add.word'
                segs.append((words[0], S('text', bg=wbg, b=self.caps.depth == '16')))   # emphasis: ink on the stronger tint
                segs += self.syntax(rest[i + len(words[0]):], bg)
            else:
                segs += self.syntax(rest, bg)
            self.out(segs, fill_bg=bg, width=W if bg else None)
        self.out([('     ' + self.g['more'] + ' 34 more lines' + self.sep() + 'ctrl-o', S('text.faint'))])

    def syntax(self, code, bg):
        out = []
        for tokn in re.findall(r'\s+|[A-Za-z_][A-Za-z0-9_]*|\S', code):
            if tokn in ('fn', 'let', 'mut', 'pub', 'impl'):
                st = 'syn.keyword'
            elif tokn[:1].isupper():
                st = 'syn.type'
            elif tokn.isdigit():
                st = 'syn.number'
            else:
                st = 'text'
            out.append((tokn, S(st, bg=bg)))
        return out

    def approval(self):
        g, sep = self.g, self.sep()
        self.blank()
        rail = (g['approval_rail'] + ' ', S('warn.glyph'))
        self.out([(' ', None), rail, (' Needs you ', S('warn.on', bg='warn.fill', b=True, chip=True)),
                  ('  Send', S('text.muted'))])
        self.out([(' ', None), rail, ('Push 3 commits and open a pull request', S('text', b=True)),
                  (' on GitHub?', S('text'))])
        for k_, v in [('Repository', 'example-org/website-redesign'), ('Branch', 'feat/update-layout into main'),
                      ('Commits', '3' + sep + '4 files' + sep + '+167 %s1' % g['minus']),
                      ('Runs', 'git push' + sep + 'gh pr create')]:
            self.out([(' ', None), rail, ('  ' + k_.ljust(12), S('text.muted')), (v, S('text'))])
        self.out([(' ', None), rail])
        opts = [('Approve', 'y', True), ('Approve for this task', 't', False), ('Edit', 'e', False),
                ('Not now', 'n  esc', False)]
        for label, key, sel in opts:
            mark = (g['prompt'] + ' ', S('warn.glyph')) if sel else ('  ', None)
            self.out([(' ', None), rail, mark, (label.ljust(26), S('text', b=sel, r=sel and self.caps.depth == 'none')),
                      (key, S('text.faint'))])
        self.out([(' ', None), rail, ('Pushing needs a keypress here. "Yes" typed in the chat doesn\'t count.',
                                      S('text.muted'))])

    def live_region(self):
        g, sep, W = self.g, self.sep(), self.W
        self.blank()
        self.out([(' ', None), ('Live region at the bottom (the only part that redraws):', S('text.faint'))])
        self.blank()
        self.out([(' ', None), (g['prompt'] + ' ', S('accent')), ('Ask for a change, or type / for commands',
                                                                  S('text.faint'))], fill_bg='surface', width=W)
        left = [(' ', None)] + self.orb_segs(self.orb.frame('rest', 0)) + [('  Ready', S('text.faint'))]
        right = 'sonnet-5.5' + sep + 'your Claude plan' + sep + '64% context left' + sep + '$0.21' + sep + \
            'cache 94%'
        pad = W - text_width(''.join(x for x, _ in left)) - text_width(right)
        if pad < 2:
            right = '64% context left' + sep + '$0.21'
            pad = W - text_width(''.join(x for x, _ in left)) - text_width(right)
        self.out(left + [(' ' * max(1, pad), None), (right, S('text.muted'))])

    # ── widths ──
    def widths(self):
        self.title('Check:', 'glyph widths in this terminal')
        orb = set(''.join(ring_cells(on)[0] + ring_cells(on)[1] for on in
                          [REST, ALL] + [ALL - {s, s + 4} for s in range(4)]))
        glyphs = sorted((set(''.join(UNICODE.values())) | set(''.join(NARROW.values())) | set(CHECK) | orb |
                         set(''.join(SPLASH_REST))) - {' '} - set(
                             'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789|`-.>'))
        expected = {c: cell_width(c) for c in glyphs}
        amb = [c for c in glyphs if unicodedata.east_asian_width(c) == 'A']
        if self.caps.ascii:
            self.note('   ASCII glyphs only, so there is nothing to measure.', 'text.muted')
            return
        if not (self.caps.tty and self.caps.keys):
            self.note('   Not a terminal, so nothing was measured. By Unicode data: %d glyphs, all one cell; '
                      'East Asian ambiguous: %s' % (len(glyphs), ' '.join(amb)), 'text.muted')
            return
        bad, unknown = [], []
        for c in glyphs:
            self.term.write('\r\x1b[2K' + c + '\x1b[6n')
            self.term.flush()
            buf = self.term.read_until(rb'\x1b\[\d+;\d+R', 0.3)
            m = re.search(rb'\x1b\[(\d+);(\d+)R', buf)
            self.term.write('\r\x1b[2K')
            if not m:
                unknown.append(c)
                if len(unknown) == 1 and c == glyphs[0]:
                    break             # the terminal doesn't answer cursor-position reports at all
                continue
            w = int(m.group(2)) - 1
            if w != expected[c]:
                bad.append((c, w))
        self.term.flush()
        if unknown and unknown[0] == glyphs[0] and len(unknown) == 1 and not bad:
            self.note('   This terminal doesn\'t answer cursor-position reports, so nothing was measured.',
                      'text.muted')
            unknown = []
        elif bad:
            self.note('   Mismatches: ' + ', '.join('%s is %d cells' % (c, w) for c, w in bad), 'error')
            self.note('   Try --ambiguous-wide or --ascii, or set ambiguous width to narrow in the terminal.',
                      'text.muted')
        else:
            self.out([('   All %d glyphs advance one cell here.' % (len(glyphs) - len(unknown)), S('ok'))])
        if unknown:
            self.note('   No reply for: ' + ' '.join(unknown))
        self.note('   East Asian ambiguous, replaced under --ambiguous-wide: ' + ' '.join(amb))


SECTIONS = ['intro', 'palette', 'wordmark', 'reveal', 'states', 'first-token', 'tools', 'notify', 'tree',
            'chat', 'widths']
ALIAS_SECTIONS = {'orb': 'states', 'motion': None, 'splash': 'reveal', 'colour': 'palette', 'color': 'palette'}


def main(argv=None):
    ap = argparse.ArgumentParser(description='Preview Wisp\'s terminal brand: palette, mark, motion and a chat turn.')
    ap.add_argument('--section', action='append', help='run only these sections (see --list)')
    ap.add_argument('--list', action='store_true', help='list the sections and exit')
    ap.add_argument('--reduced-motion', action='store_true', help='static frames only (same as --motion reduced)')
    ap.add_argument('--motion', choices=['full', 'calm', 'reduced'], help='overrides WISP_MOTION')
    ap.add_argument('--theme', choices=['auto', 'dark', 'light'], default='auto')
    ap.add_argument('--colors', choices=['auto', 'truecolor', '256', '16', 'none'], default='auto',
                    help='colour depth; auto honours NO_COLOR, COLORTERM and TERM')
    ap.add_argument('--ascii', action='store_true', help='ASCII glyphs only')
    ap.add_argument('--ambiguous-wide', action='store_true',
                    help='the terminal draws East Asian ambiguous characters two cells wide')
    ap.add_argument('--quick', action='store_true', help='shorter animations')
    ap.add_argument('--no-probe', action='store_true', help='don\'t query the terminal')
    args = ap.parse_args(argv)
    if args.list:
        print('\n'.join(SECTIONS))
        return 0
    wanted = SECTIONS
    if args.section:
        wanted = []
        for s in args.section:
            s = ALIAS_SECTIONS.get(s, s)
            if s is None:
                wanted += ['reveal', 'states', 'first-token', 'tools', 'notify', 'tree']
            elif s in SECTIONS:
                wanted.append(s)
            else:
                ap.error('unknown section %r (try --list)' % s)

    caps = detect(args)
    caps.quick = args.quick

    def bail(signum, frame):
        raise SystemExit(128 + signum)
    for sig in ('SIGTERM', 'SIGHUP'):
        if hasattr(signal, sig):
            signal.signal(getattr(signal, sig), bail)

    term = Term(caps)
    try:
        with term:
            if caps.keys and not args.no_probe:
                probe(term, caps)
            caps.theme = pick_theme(args, caps)
            tok, corrected = resolve_tokens(caps.theme, caps.bg, caps.fg)
            pv = Preview(term, caps, tok, corrected)
            for name in wanted:
                try:
                    getattr(pv, name.replace('-', '_'))()
                except Skip:
                    pass
            term.write('\n')
            term.flush()
    except Quit:
        term.restore()
        sys.stdout.write('\n')
    except KeyboardInterrupt:
        term.restore()
        sys.stdout.write('\n')
        return 130
    finally:
        term.restore()
    return 0


if __name__ == '__main__':
    sys.exit(main())
