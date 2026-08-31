#!/usr/bin/env python3
"""Render a Herdr pane's ANSI output as an SVG.

    herdr pane read <pane> --source visible --format ansi | scripts/termshot.py > doc/demo.svg

The point is that the picture in the README is the real thing: whatever the
terminal actually had on screen, cell for cell, rather than a mock-up that
drifts from the code. No dependencies, so regenerating it needs nothing but
python3.
"""
import html
import re
import sys

SGR = re.compile(r"\x1b\[([0-9;]*)m")
FONT = (
    "ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, "
    "'DejaVu Sans Mono', monospace"
)
CW, CH, PAD = 8.4, 18.0, 16.0  # cell width, line height, padding
DEFAULT_BG = "#1e1e2e"
DEFAULT_FG = "#cdd6f4"


def parse(text):
    """ANSI text -> list of lines, each a list of (string, fg, bg, bold)."""
    lines, run = [], []
    fg = bg = None
    bold = False
    buf = []

    def flush():
        if buf:
            run.append(("".join(buf), fg, bg, bold))
            buf.clear()

    i = 0
    while i < len(text):
        m = SGR.match(text, i)
        if m:
            flush()
            codes = [int(c) for c in m.group(1).split(";") if c != ""] or [0]
            j = 0
            while j < len(codes):
                c = codes[j]
                if c == 0:
                    fg = bg = None
                    bold = False
                elif c == 1:
                    bold = True
                elif c == 22:
                    bold = False
                elif c in (38, 48) and codes[j + 1 : j + 2] == [2]:
                    rgb = "#%02x%02x%02x" % tuple(codes[j + 2 : j + 5])
                    if c == 38:
                        fg = rgb
                    else:
                        bg = rgb
                    j += 4
                j += 1
            i = m.end()
            continue
        ch = text[i]
        if ch == "\n":
            flush()
            lines.append(run)
            run = []
        elif ch != "\r":
            buf.append(ch)
        i += 1
    flush()
    if run:
        lines.append(run)
    return lines


def render(lines, title):
    cols = max((sum(len(s) for s, *_ in ln) for ln in lines), default=80)
    bar = 28.0
    w = cols * CW + PAD * 2
    h = len(lines) * CH + PAD * 2 + bar
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{w:.0f}" height="{h:.0f}" '
        f'viewBox="0 0 {w:.0f} {h:.0f}" font-family="{FONT}" font-size="13">',
        f'<rect width="{w:.0f}" height="{h:.0f}" rx="8" fill="{DEFAULT_BG}"/>',
    ]
    for n, cx in enumerate((16, 32, 48)):
        out.append(
            f'<circle cx="{cx}" cy="14" r="5" fill="{["#f38ba8","#f9e2af","#a6e3a1"][n]}"/>'
        )
    if title:
        out.append(
            f'<text x="{w/2:.0f}" y="19" fill="#6c7086" text-anchor="middle" '
            f'font-size="12">{html.escape(title)}</text>'
        )
    for row, runs in enumerate(lines):
        y = PAD + bar + row * CH + CH * 0.72
        col = 0
        for s, fg, bg, bold in runs:
            x = PAD + col * CW
            if bg:
                out.append(
                    f'<rect x="{x:.1f}" y="{y - CH * 0.72:.1f}" '
                    f'width="{len(s) * CW:.1f}" height="{CH:.1f}" fill="{bg}"/>'
                )
            if s.strip():
                weight = ' font-weight="600"' if bold else ""
                out.append(
                    f'<text x="{x:.1f}" y="{y:.1f}" fill="{fg or DEFAULT_FG}"'
                    f'{weight} xml:space="preserve">{html.escape(s)}</text>'
                )
            col += len(s)
    out.append("</svg>")
    return "\n".join(out)


if __name__ == "__main__":
    title = sys.argv[1] if len(sys.argv) > 1 else ""
    raw = sys.stdin.buffer.read().decode("utf-8", "replace")
    body = [ln for ln in parse(raw)]
    # Trim the blank rows a terminal snapshot pads with.
    while body and not any(s.strip() for s, *_ in body[0]):
        body.pop(0)
    while body and not any(s.strip() for s, *_ in body[-1]):
        body.pop()
    sys.stdout.write(render(body, title))
