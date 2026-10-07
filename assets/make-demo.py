#!/usr/bin/env python3
"""Regenerates assets/demo.svg, the animated terminal demo in the README.

It runs the real binary in a temporary folder, so the demo always shows
what the tool actually prints:

    cargo build --release && python3 assets/make-demo.py
"""

import os
import subprocess
import tempfile
from html import escape
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY_DIR = ROOT / "target" / "release"

SOURCE = """\
fn main() {
    // So we're doing something complicated here, long enough
    // that we need multiple lines of comments to do it!
    let lucky_number = 7; // I'm feeling lucky today
}
"""

COMMANDS = [
    "cat main.rs",
    "codecleanup --in=main.rs --docs=docs/",
    "cat main.rs",
    "head -n 6 docs/main.md",
    "codecleanup --in=main.rs --docs=docs/ --restore",
    "cat main.rs",
]

# Seconds: from a command to its output, from the output to the next
# command, and how long the finished screen stays before the loop restarts.
OUTPUT_DELAY, COMMAND_DELAY, HOLD = 0.7, 2.2, 5.0

WIDTH, LINE, LEFT, TOP = 720, 19, 18, 58
COLORS = {
    "background": "#0d1117",
    "bar": "#161b22",
    "text": "#c9d1d9",
    "prompt": "#3fb950",
    "command": "#f0f6fc",
    "comment": "#8b949e",
    "refid": "#e3b341",
    "heading": "#58a6ff",
}


def run(command: str, cwd: str) -> list[str]:
    env = dict(os.environ, PATH=f"{BINARY_DIR}{os.pathsep}{os.environ['PATH']}")
    result = subprocess.run(command, shell=True, cwd=cwd, env=env, capture_output=True, text=True, check=True)
    return result.stdout.rstrip("\n").split("\n")


def span(text: str, color: str, bold: bool = False) -> str:
    weight = ' font-weight="bold"' if bold else ""
    return f'<tspan fill="{COLORS[color]}"{weight}>{escape(text)}</tspan>'


def colored(line: str) -> str:
    """Highlights the parts of an output line that the demo is about."""
    if line.startswith("## "):
        return span(line, "heading")
    code, slashes, comment = line.partition("//")
    if not slashes:
        return span(line, "text")
    return span(code, "text") + span(slashes + comment, "refid" if "refid:" in comment else "comment")


def main() -> None:
    # Each entry: the time it appears at, and its lines as SVG markup.
    blocks: list[tuple[float, list[str]]] = []
    now = 0.8
    with tempfile.TemporaryDirectory() as cwd:
        Path(cwd, "main.rs").write_text(SOURCE)
        for command in COMMANDS:
            blocks.append((now, [span("$ ", "prompt") + span(command, "command", bold=True)]))
            blocks.append((now + OUTPUT_DELAY, [colored(line) for line in run(command, cwd)]))
            now += COMMAND_DELAY
    total = now + HOLD

    rows = sum(len(lines) for _, lines in blocks)
    height = TOP + rows * LINE + 14
    styles, texts, row = [], [], 0
    for number, (start, lines) in enumerate(blocks):
        at = 100 * start / total
        styles.append(
            f"@keyframes b{number} {{ 0%, {at - 0.01:.2f}% {{ opacity: 0 }} {at:.2f}%, 100% {{ opacity: 1 }} }}\n"
            f".b{number} {{ animation: b{number} {total:.1f}s linear infinite }}"
        )
        texts.append(f'<g class="b{number}">')
        for line in lines:
            y = TOP + row * LINE
            texts.append(f'<text x="{LEFT}" y="{y}" xml:space="preserve">{line}</text>')
            row += 1
        texts.append("</g>")

    svg = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {height}" width="{WIDTH}" height="{height}" role="img" aria-label="Terminal session: codecleanup replaces the comments in main.rs with refids and restores them">
<style>
text {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, "DejaVu Sans Mono", "Liberation Mono", monospace; font-size: 14px }}
{chr(10).join(styles)}
</style>
<rect width="{WIDTH}" height="{height}" rx="8" fill="{COLORS['background']}"/>
<path d="M0 8a8 8 0 0 1 8-8h{WIDTH - 16}a8 8 0 0 1 8 8v22H0z" fill="{COLORS['bar']}"/>
<circle cx="18" cy="15" r="6" fill="#ff5f56"/><circle cx="38" cy="15" r="6" fill="#ffbd2e"/><circle cx="58" cy="15" r="6" fill="#27c93f"/>
<text x="{WIDTH // 2}" y="20" text-anchor="middle" fill="{COLORS['comment']}">codecleanup</text>
{chr(10).join(texts)}
</svg>
"""
    (ROOT / "assets" / "demo.svg").write_text(svg)
    print(f"wrote assets/demo.svg, {rows} lines, loops every {total:.1f} s")


if __name__ == "__main__":
    main()
