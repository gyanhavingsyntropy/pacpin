"""Run a controlled Rust PTY fixture through Kitty's screen engine.

Invoke via kitty +runpy; this supplements, and does not replace, real VM checks.
"""
import argparse
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import termios
import time
from kitty.fast_data_types import Screen

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True, help='Rust unit-test executable from target/debug/deps')
parser.add_argument('--output', type=Path, required=True, help='Fresh evidence directory')
args = parser.parse_args()
binary = args.binary.resolve()
output = args.output.resolve()
output.mkdir(parents=True, exist_ok=False)
master, slave = pty.openpty()
fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 80, 0, 0))
def controlling_terminal():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)
env = dict(os.environ, PACPIN_PROGRESS_PTY_TEST='1', TERM='xterm-kitty')
child = subprocess.Popen([str(binary), '--exact', 'progress::tests::pty_preserves_prompt_output_footer_and_terminal_settings', '--nocapture'], stdin=slave, stdout=slave, stderr=slave, env=env, preexec_fn=controlling_terminal)
os.close(slave)
screen = Screen(None, 30, 80, 10000)
def text(history=False):
    parts = []
    if history:
        screen.as_text_for_history_buf(parts.append)
    screen.as_text(parts.append)
    return ''.join(parts)
raw = bytearray()
frames = []
started = None
secret_sent = answer_sent = selection_sent = False
deadline = time.monotonic() + 20
result = {'kind': 'Kitty Screen engine with controlled PTY fixture, not a guest CLI test', 'passed': False}
def require(condition, message):
    if not condition:
        raise AssertionError(message)
try:
    while True:
        if time.monotonic() > deadline:
            raise AssertionError('fixture timeout')
        if started is not None and not secret_sent and time.monotonic() - started > .75:
            rendered = text()
            (output / 'password-wait.screen.txt').write_text(rendered)
            require('AUTH_WAIT_PROMPT:' in rendered, 'waiting authentication prompt erased after resize')
            os.write(master, b'fixture-secret\n')
            secret_sent = True
        ready, _, _ = select.select([master], [], [], .05)
        if not ready:
            continue
        try:
            data = os.read(master, 8192)
        except OSError as error:
            if error.errno == errno.EIO:
                break
            raise
        if not data:
            break
        raw.extend(data)
        buffer = screen.test_create_write_buffer()
        screen.test_commit_write_buffer(data, buffer)
        screen.test_parse_written_data()
        frames.append(text())
        if started is None and b'AUTH_WAIT_PROMPT: ' in raw:
            started = time.monotonic()
            screen.resize(18, 40)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 18, 40, 0, 0))
        if not answer_sent and b'Answer: ' in raw:
            os.write(master, b'yes\n')
            answer_sent = True
            screen.resize(30, 100)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 0, 0))
        if not selection_sent and b'Enter confirms' in raw:
            os.write(master, b'\r')
            selection_sent = True
    require(child.wait(timeout=2) == 0, 'fixture failed')
    final = text(history=True)
    for marker in ['PHASE_PADDING_START', 'PHASE_PADDING_END', 'AUTH_WAIT_COMPLETE', 'native output retained', 'stderr retained', 'source terminal retained', 'raw output line one', 'raw output line two', 'BACKGROUND_WAIT_FINISHED']:
        require(marker in final, f'lost screen/scrollback: {marker}')
    require('fixture-secret' not in final, 'secret echoed')
    require(secret_sent and answer_sent and selection_sent, 'fixture input incomplete')
    result.update(passed=True, frames=len(frames), prompt_after_resize=True, scrollback_retained=True)
    print(json.dumps(result))
finally:
    (output / 'results.json').write_text(json.dumps(result, indent=2))
    (output / 'fixture.raw.txt').write_bytes(raw)
    (output / 'fixture.screen.txt').write_text(text(history=True))
    (output / 'frames.json').write_text(json.dumps(frames))
    if child.poll() is None:
        os.killpg(child.pid, signal.SIGTERM)
        child.wait(timeout=2)
    os.close(master)
