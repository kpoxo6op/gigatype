#!/usr/bin/env python3
"""Minimal real X11 paste target for the GitHub-hosted E2E test."""

import pathlib
import sys
import time
import tkinter as tk


OUTPUT = pathlib.Path(sys.argv[1])
STARTED = time.monotonic()
last_text = ""
stable_since = STARTED

root = tk.Tk()
root.title("GigaType-E2E")
editor = tk.Text(root, width=100, height=8)
editor.pack()
root.update()
root.lift()
editor.focus_force()


def save_when_stable() -> None:
    global last_text, stable_since
    text = editor.get("1.0", "end-1c")
    now = time.monotonic()
    if text != last_text:
        last_text = text
        stable_since = now
    if text and now - stable_since >= 0.5:
        OUTPUT.write_text(text, encoding="utf-8")
        root.destroy()
        return
    if now - STARTED >= 120:
        OUTPUT.write_text(text, encoding="utf-8")
        root.destroy()
        return
    root.after(50, save_when_stable)


root.after(50, save_when_stable)
root.mainloop()
