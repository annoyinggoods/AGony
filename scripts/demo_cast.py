#!/usr/bin/env python3
"""Capture a terminal program to an asciicast, with scripted keystrokes.

Part10 §10.4.2's recording pipeline for this box: neither vhs (hangs
before spawning ttyd here) nor `terminal-svg rec` (Unix-only) can record
on Windows, so this script drives the program through a real ConPTY
(pywinpty) — the same hosting rmux uses — and writes asciicast v2.
`terminal-svg <file>.cast -o <file>.svg` renders the SVG afterwards.

    python scripts/demo_cast.py --out demo/desktop.cast \
        --script "38:ctrl-q" -- C:\\Projects\\AGony\\target\\debug\\agony.exe

The --script is a semicolon list of `SECONDS:ACTION`; actions are
`type:<text>`, or one of enter / esc / tab / ctrl-k / ctrl-q / ctrl-g /
ctrl-i (crossterm reads Tab as Ctrl-I in the console input) /
ctrl-space. Keystrokes are written into the ConPTY input, so a TUI
reads them like real keys (crossterm sees them through the console
input buffer).
"""

import argparse
import json
import os
import sys
import threading
import time

from winpty import PtyProcess

KEYS = {
    "enter": "\r",
    "esc": "\x1b",
    "tab": "\t",
    "up": "\x1b[A",
    "down": "\x1b[B",
    "right": "\x1b[C",
    "left": "\x1b[D",
    "ctrl-k": "\x0b",
    "ctrl-q": "\x11",
    "ctrl-g": "\x07",
    "ctrl-t": "\x14",
    "ctrl-space": "\x00",
    # ConPTY parses xterm's modified-key sequence into a real key record
    # with the CONTROL flag — a bare 0x09 byte arrives as plain Tab.
    "ctrl-tab": "\x1b[27;5;9~",
}


def parse_script(spec: str):
    steps = []
    for part in filter(None, (chunk.strip() for chunk in spec.split(";"))):
        at, action = part.split(":", 1)
        steps.append((float(at), action))
    return sorted(steps)


def key_bytes(action: str) -> str:
    if action.startswith("type:"):
        return action[len("type:") :]
    if action in KEYS:
        return KEYS[action]
    raise SystemExit(f"unknown script action: {action}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, help="asciicast v2 output path")
    parser.add_argument("--cols", type=int, default=120)
    parser.add_argument("--rows", type=int, default=34)
    parser.add_argument("--title", default="")
    parser.add_argument(
        "--appdata",
        default="",
        help="override APPDATA for the child (an isolated config dir)",
    )
    parser.add_argument(
        "--cwd",
        default="",
        help="the child's working directory (the file panel's root)",
    )
    parser.add_argument("--script", default="", help='e.g. "10:ctrl-k;11:type:git che;18:ctrl-q"')
    parser.add_argument("--deadline", type=float, default=90.0, help="hard stop in seconds")
    parser.add_argument("command", nargs="+", help="the command to record")
    args = parser.parse_args()

    env = os.environ.copy()
    if args.appdata:
        env["APPDATA"] = args.appdata

    command = " ".join(args.command)
    cwd = args.cwd or None
    proc = PtyProcess.spawn(command, cwd=cwd, dimensions=(args.rows, args.cols), env=env)

    events: list[tuple[float, str]] = []
    started = time.time()

    def reader() -> None:
        while proc.isalive():
            try:
                data = proc.read()
            except (EOFError, OSError):
                break
            if not data:
                break
            events.append((time.time(), data))

    reader_thread = threading.Thread(target=reader, daemon=True)
    reader_thread.start()

    for at, action in parse_script(args.script):
        remaining = at - (time.time() - started)
        if remaining > 0:
            time.sleep(remaining)
        if proc.isalive():
            proc.write(key_bytes(action))

    while proc.isalive() and time.time() - started < args.deadline:
        time.sleep(0.2)
    if proc.isalive():
        proc.terminate(force=True)
    time.sleep(0.3)
    reader_thread.join(timeout=2.0)

    duration = max((t for t, _ in events), default=started) - started
    with open(args.out, "w", encoding="utf-8", newline="\n") as cast:
        header = {
            "version": 2,
            "width": args.cols,
            "height": args.rows,
            "timestamp": int(started),
            "duration": round(duration, 3),
            "title": args.title or command,
            "env": {"SHELL": os.environ.get("SHELL", "powershell"), "TERM": "xterm-256color"},
        }
        cast.write(json.dumps(header) + "\n")
        for at, data in events:
            # terminal-svg's event line: a [time, code, data] triple.
            event = [round(at - started, 3), "o", data]
            cast.write(json.dumps(event, ensure_ascii=False) + "\n")
    print(f"{args.out}: {len(events)} chunks over {duration:.1f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
