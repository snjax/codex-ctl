#!/usr/bin/env python3
"""Minimal test: just print text and ANSI, see what VT100 parser captures."""
import sys
import time

def write(s):
    sys.stdout.write(s)
    sys.stdout.flush()

# Phase 1: plain text
write("Phase 1: plain text\n")
write("  • Working on task...\n")
write("  esc to interrupt\n")
time.sleep(1)

# Phase 2: more content
write("  • Edited main.rs (+5 -2)\n")
write("  • Ran cargo test\n")
time.sleep(1)

# Phase 3: cursor positioning
write("\033[H")  # home
write("\033[J")  # clear screen
write("\033[1;1H")  # move to row 1, col 1
write("After clear:\n")
write("  • Created hello.py (+1 -0)\n")
write("  esc to interrupt\n")
time.sleep(1)

# Phase 4: idle
write("\033[H\033[J")
write("  • Created hello.py (+1 -0)\n")
write("  Done.\n")
time.sleep(2)
