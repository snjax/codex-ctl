#!/usr/bin/env python3
"""
Dummy TUI app that mimics OpenAI Codex CLI rendering for testing codex-ctl.

Renders inline (no alternate screen) using scroll regions and cursor positioning,
similar to how codex --no-alt-screen renders its TUI.

Usage:
    python3 dummy_codex.py [--scenario SCENARIO] [PROMPT]

Scenarios:
    fast     - Quick task, completes in ~2s (default)
    slow     - Longer task with progress, ~8s
    prompt   - Asks a question, waits for user input, then completes
    multi    - Multiple questions before completing
    idle     - Works then goes idle, stays running
    error    - Simulates an error during execution

The app renders:
    - Status line with "esc to interrupt" while working
    - Agent messages with bullet points (•)
    - Block headers like "• Edited file.py (+3 -1)"
    - Question prompts with numbered options
    - Exit animation on completion

Keyboard:
    Enter       - Submit during prompting
    Up/Down     - Navigate options during prompting
    Esc         - Interrupt during working
    q           - Quit
"""

import sys
import os
import time
import select
import termios
import tty
import argparse
import signal

# ── ANSI helpers ──────────────────────────────────────────────────────────

def write(s):
    sys.stdout.write(s)
    sys.stdout.flush()

def csi(code):
    write(f"\033[{code}")

def move_to(row, col):
    csi(f"{row};{col}H")

def erase_line():
    csi("2K")

def erase_below():
    csi("J")

def set_scroll_region(top, bottom):
    csi(f"{top};{bottom}r")

def reset_scroll_region():
    csi("r")

def save_cursor():
    csi("s")

def restore_cursor():
    csi("u")

def bold(text):
    return f"\033[1m{text}\033[0m"

def dim(text):
    return f"\033[2m{text}\033[0m"

def italic(text):
    return f"\033[3m{text}\033[0m"

def cyan(text):
    return f"\033[38;5;6m{text}\033[0m"

def green(text):
    return f"\033[38;5;2m{text}\033[0m"

def red(text):
    return f"\033[38;5;1m{text}\033[0m"

def yellow(text):
    return f"\033[38;5;3m{text}\033[0m"


# ── Terminal I/O ──────────────────────────────────────────────────────────

class RawTerminal:
    def __init__(self):
        self.fd = sys.stdin.fileno()
        self.old_settings = None

    def __enter__(self):
        self.old_settings = termios.tcgetattr(self.fd)
        tty.setraw(self.fd)
        return self

    def __exit__(self, *args):
        if self.old_settings:
            termios.tcsetattr(self.fd, termios.TCSADRAIN, self.old_settings)

    def read_key(self, timeout=0.05):
        """Read a key with timeout. Returns None if no key available."""
        if select.select([sys.stdin], [], [], timeout)[0]:
            ch = sys.stdin.read(1)
            if ch == '\x1b':
                # Check for escape sequence
                if select.select([sys.stdin], [], [], 0.02)[0]:
                    ch2 = sys.stdin.read(1)
                    if ch2 == '[':
                        ch3 = sys.stdin.read(1)
                        if ch3 == 'A': return 'UP'
                        if ch3 == 'B': return 'DOWN'
                        if ch3 == 'C': return 'RIGHT'
                        if ch3 == 'D': return 'LEFT'
                    return 'ESC'
                return 'ESC'
            if ch == '\r' or ch == '\n': return 'ENTER'
            if ch == '\t': return 'TAB'
            if ch == '\x03': return 'CTRL_C'
            if ch == '\x04': return 'CTRL_D'
            if ch == 'q': return 'q'
            return ch
        return None


# ── Screen renderer ───────────────────────────────────────────────────────

class Screen:
    """Renders a TUI similar to codex --no-alt-screen inline mode."""

    def __init__(self):
        try:
            rows, cols = os.get_terminal_size()
        except OSError:
            rows, cols = 500, 200  # PTY defaults
        self.rows = rows
        self.cols = cols
        self.content_lines = []
        self.status_line = ""
        self.start_row = 1

    def render_frame(self):
        """Full redraw of the TUI frame."""
        # Set scroll region for content area
        set_scroll_region(1, self.rows)
        move_to(1, 1)
        erase_below()

        # Render content lines
        for i, line in enumerate(self.content_lines):
            move_to(self.start_row + i, 1)
            erase_line()
            write(line)

        # Render status line at bottom
        if self.status_line:
            move_to(self.rows, 1)
            erase_line()
            write(self.status_line)

        # Park cursor
        content_end = self.start_row + len(self.content_lines)
        move_to(content_end, 1)

    def set_content(self, lines):
        self.content_lines = lines
        self.render_frame()

    def append_content(self, line):
        self.content_lines.append(line)
        row = self.start_row + len(self.content_lines) - 1
        move_to(row, 1)
        erase_line()
        write(line)
        # Update status line position
        if self.status_line:
            move_to(self.rows, 1)
            erase_line()
            write(self.status_line)

    def set_status(self, text):
        self.status_line = text
        move_to(self.rows, 1)
        erase_line()
        write(text)

    def clear(self):
        self.content_lines = []
        self.status_line = ""
        set_scroll_region(1, self.rows)
        move_to(1, 1)
        erase_below()


# ── Scenarios ─────────────────────────────────────────────────────────────

def scenario_fast(screen, term, prompt):
    """Quick task: working → done → idle."""

    # Working phase
    screen.set_status(dim("esc to interrupt"))
    screen.append_content("")
    screen.append_content(dim(f"  {italic('Analyzing request...')}"))
    time.sleep(0.5)

    screen.set_content([
        "",
        f"  {dim('•')} {bold('Creating')} {cyan('hello.py')} in current directory",
    ])
    time.sleep(0.3)

    # Block: file created
    screen.set_content([
        "",
        f"  {dim('•')} {bold('Created')} {cyan('hello.py')} {dim(f'({green('+1')} {red('-0')})')}",
        f"     {dim('1')}  {green(f'+print(\"Hello, World!\")')}",
        "",
        f"  {dim('•')} {bold('Ran')} {cyan('python hello.py')}",
        f"     {dim('│')} {dim('Hello, World!')}",
        "",
        f"  File created and verified.",
        "",
    ])
    screen.set_status(dim("esc to interrupt"))
    time.sleep(1.0)

    # Idle
    screen.set_status(dim("task complete"))
    time.sleep(0.3)

    # Clear status - go idle
    screen.set_status("")
    time.sleep(5.0)


def scenario_slow(screen, term, prompt):
    """Longer task with progress updates."""

    screen.set_status(dim("esc to interrupt"))

    messages = [
        (0.5, f"  {italic('Reading codebase...')}"),
        (1.0, f"  {dim('•')} Analyzing project structure"),
        (1.5, f"  {dim('•')} Found 3 relevant files"),
        (1.0, f"  {dim('•')} {bold('Edited')} {cyan('src/main.rs')} {dim(f'({green('+15')} {red('-3')})')}"),
        (0.5, f"     {dim('│')} Added error handling to parse_args()"),
        (0.5, f"     {dim('│')} Updated return type to Result<()>"),
        (0.5, f"     {dim('└')} {dim('…+8 lines')}"),
        (1.0, f"  {dim('•')} {bold('Edited')} {cyan('src/lib.rs')} {dim(f'({green('+5')} {red('-2')})')}"),
        (0.5, f"     {dim('│')} Added new helper function"),
        (0.5, f"     {dim('└')} {dim('…+2 lines')}"),
        (1.0, f"  {dim('•')} {green(bold('Ran'))} {cyan('cargo test')}"),
        (0.5, f"     {dim('│')} running 12 tests"),
        (0.5, f"     {dim('│')} test result: ok. 12 passed"),
        (0.5, f"     {dim('└')} {dim('…+8 lines')}"),
        (0.5, ""),
        (0.5, f"  All changes applied and tests pass."),
    ]

    screen.set_content([""])
    for delay, msg in messages:
        key = term.read_key(timeout=delay)
        if key == 'ESC':
            screen.append_content("")
            screen.append_content(f"  {yellow('Interrupted by user')}")
            screen.set_status("")
            time.sleep(5.0)
            return
        screen.append_content(msg)

    screen.set_status("")
    time.sleep(5.0)


def scenario_prompt(screen, term, prompt):
    """Working → prompting → working → idle."""

    # Working phase
    screen.set_status(dim("esc to interrupt"))
    screen.set_content([
        "",
        f"  {italic('Analyzing request...')}",
    ])
    time.sleep(1.0)

    screen.set_content([
        "",
        f"  {dim('•')} Found multiple approaches for this task",
        "",
    ])
    time.sleep(0.5)

    # Prompting phase
    options = [
        "Create new file from scratch",
        "Modify existing file",
        "Use template",
    ]
    selected = 0

    def render_prompt(sel):
        lines = [
            "",
            f"  {dim('•')} Found multiple approaches for this task",
            "",
            f"  {bold('Question 1/1')}",
            f"  Which approach would you like?",
            "",
        ]
        for i, opt in enumerate(options):
            marker = "›" if i == sel else " "
            num = f"{i+1}."
            if i == sel:
                lines.append(f"    {bold(marker)} {bold(num)} {bold(opt)}")
            else:
                lines.append(f"    {marker} {num} {opt}")
        lines.append("")
        lines.append(f"  {dim('enter to submit')}")
        return lines

    screen.set_content(render_prompt(selected))
    screen.set_status(dim("tab or esc to clear notes   enter to submit"))

    while True:
        key = term.read_key(timeout=0.1)
        if key == 'UP':
            selected = max(0, selected - 1)
            screen.set_content(render_prompt(selected))
        elif key == 'DOWN':
            selected = min(len(options) - 1, selected + 1)
            screen.set_content(render_prompt(selected))
        elif key == 'ENTER':
            break
        elif key == 'ESC' or key == 'q' or key == 'CTRL_C':
            screen.clear()
            return

    # Back to working
    chosen = options[selected]
    screen.set_status(dim("esc to interrupt"))
    screen.set_content([
        "",
        f"  {dim('•')} Selected: {bold(chosen)}",
        "",
        f"  {italic('Implementing...')}",
    ])
    time.sleep(1.5)

    screen.set_content([
        "",
        f"  {dim('•')} Selected: {bold(chosen)}",
        "",
        f"  {dim('•')} {bold('Created')} {cyan('hello.py')} {dim(f'({green('+1')} {red('-0')})')}",
        f"     {dim('1')}  {green('+print(\"Hello, World!\")')}",
        "",
        f"  Done.",
    ])
    screen.set_status("")
    time.sleep(5.0)


def scenario_multi(screen, term, prompt):
    """Multiple questions."""

    screen.set_status(dim("esc to interrupt"))
    screen.set_content([
        "",
        f"  {italic('Planning...')}",
    ])
    time.sleep(0.8)

    for q_num in range(1, 3):
        options = [
            ["Python", "Rust", "Go"] if q_num == 1 else ["MIT", "Apache-2.0", "GPL-3.0"],
        ][0]
        question = "Which language?" if q_num == 1 else "Which license?"
        selected = 0

        def render_q(sel, qn=q_num, opts=options, qtext=question):
            lines = [
                "",
                f"  {bold(f'Question {qn}/2')}",
                f"  {qtext}",
                "",
            ]
            for i, opt in enumerate(opts):
                marker = "›" if i == sel else " "
                num = f"{i+1}."
                if i == sel:
                    lines.append(f"    {bold(marker)} {bold(num)} {bold(opt)}")
                else:
                    lines.append(f"    {marker} {num} {opt}")
            lines.append("")
            lines.append(f"  {dim('enter to submit')}")
            return lines

        screen.set_content(render_q(selected))
        screen.set_status(dim("tab or esc to clear notes   enter to submit"))

        while True:
            key = term.read_key(timeout=0.1)
            if key == 'UP':
                selected = max(0, selected - 1)
                screen.set_content(render_q(selected))
            elif key == 'DOWN':
                selected = min(len(options) - 1, selected + 1)
                screen.set_content(render_q(selected))
            elif key == 'ENTER':
                break
            elif key == 'ESC' or key == 'q' or key == 'CTRL_C':
                screen.clear()
                return

    # Complete
    screen.set_status(dim("esc to interrupt"))
    screen.set_content([
        "",
        f"  {dim('•')} {bold('Created')} {cyan('project/')} with selected options",
        "",
        f"  Project initialized.",
    ])
    time.sleep(1.0)
    screen.set_status("")
    time.sleep(5.0)


def scenario_idle(screen, term, prompt):
    """Work then stay idle indefinitely."""

    screen.set_status(dim("esc to interrupt"))
    screen.set_content([
        "",
        f"  {italic('Working...')}",
    ])
    time.sleep(1.5)

    screen.set_content([
        "",
        f"  {dim('•')} {bold('Created')} {cyan('hello.py')} {dim(f'({green('+1')} {red('-0')})')}",
        "",
        f"  Done. Waiting for next instruction.",
    ])
    screen.set_status("")

    # Stay alive, idle
    while True:
        key = term.read_key(timeout=1.0)
        if key == 'q' or key == 'CTRL_C' or key == 'CTRL_D':
            break


def scenario_error(screen, term, prompt):
    """Simulates an error."""

    screen.set_status(dim("esc to interrupt"))
    screen.set_content([
        "",
        f"  {italic('Analyzing...')}",
    ])
    time.sleep(1.0)

    screen.set_content([
        "",
        f"  {dim('•')} {bold('Ran')} {cyan('cargo build')}",
        f"     {dim('│')} {red('error[E0308]: mismatched types')}",
        f"     {dim('│')}   --> src/main.rs:42:5",
        f"     {dim('│')}   expected `String`, found `&str`",
        f"     {dim('└')} {dim('…+3 lines')}",
        "",
        f"  {red('Build failed.')} Attempting fix...",
    ])
    time.sleep(1.5)

    screen.append_content("")
    screen.append_content(f"  {dim('•')} {bold('Edited')} {cyan('src/main.rs')} {dim(f'({green('+1')} {red('-1')})')}")
    screen.append_content(f"  {dim('•')} {green(bold('Ran'))} {cyan('cargo build')}")
    screen.append_content(f"     {dim('│')} Compiling myproject v0.1.0")
    screen.append_content(f"     {dim('└')} Finished dev [unoptimized] target(s)")
    screen.append_content("")
    screen.append_content(f"  Fixed and rebuilt successfully.")
    screen.set_status("")
    time.sleep(5.0)


SCENARIOS = {
    'fast': scenario_fast,
    'slow': scenario_slow,
    'prompt': scenario_prompt,
    'multi': scenario_multi,
    'idle': scenario_idle,
    'error': scenario_error,
}


# ── Main ──────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Dummy Codex TUI for testing codex-ctl")
    parser.add_argument('--scenario', '-s', default='fast', choices=SCENARIOS.keys(),
                        help='Scenario to simulate')
    parser.add_argument('prompt', nargs='?', default='test prompt',
                        help='Prompt text (ignored, for CLI compat)')
    args = parser.parse_args()

    # Handle SIGTERM gracefully
    def handle_signal(sig, frame):
        reset_scroll_region()
        move_to(999, 1)
        write("\n")
        sys.exit(0)
    signal.signal(signal.SIGTERM, handle_signal)

    screen = Screen()
    scenario_fn = SCENARIOS[args.scenario]

    with RawTerminal() as term:
        try:
            # Hide cursor
            csi("?25l")

            scenario_fn(screen, term, args.prompt)

            # Show cursor, clean up
            csi("?25h")
            reset_scroll_region()
            content_end = screen.start_row + len(screen.content_lines) + 1
            move_to(content_end, 1)
            write("\n")

        except (KeyboardInterrupt, SystemExit):
            pass
        finally:
            csi("?25h")
            reset_scroll_region()


if __name__ == '__main__':
    main()
