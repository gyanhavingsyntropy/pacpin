"""Repeated resize checks directly in the disposable guest's Kitty tty."""
import argparse
import json
import os
import time
import re
from pathlib import Path
from verify_upgrade_ui_vm import Checks, run, serialized_verifier, transcript

@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    assert os.environ.get('PACPIN_DISPOSABLE_VM') == '1' and os.geteuid() != 0
    assert 'ID=garuda' in Path('/etc/os-release').read_text()
    assert run('findmnt', '-n', '-o', 'FSTYPE', '/').strip() == 'overlay'
    assert run('systemd-detect-virt').strip() == 'oracle'
    root = args.output
    root.mkdir()
    checks = None
    try:
        checks = Checks(args.binary, root, 'unix:/tmp/pacpin-ui-kitty.sock', False, False, 'all')
        checks.direct_terminal = True
        for i in range(3):
            checks.seed()
            checks.publish(2)
            checks.catalog(2)
            checks.case(f'mixed-resize-{i}', ['-Su'], native=True, source=True, resize=True, confirm=True)
            history = transcript(root / f'mixed-resize-{i}.raw.txt')
            assert not any('PACPIN_NATIVE_HOOK_' in line and not re.fullmatch(r'PACPIN_NATIVE_HOOK_\d+', line) for line in history.splitlines()), 'Native hook output contains leftover footer text'
        checks.seed(source=False)
        checks.publish(2)
        checks.case('native-resize', ['-Su', '--noconfirm'], native=True, resize=True)
        checks.seed()
        checks.publish(1)
        checks.catalog(2)
        checks.case('sources-resize', ['-Su', '--noconfirm'], source=True, resize=True)
        checks.rc('send-text', '--stdin', input="printf '\\033[2J\\033[HWRAP_CHECK_" + 'W' * 70 + "\\n'\n")
        time.sleep(.3)
        screen = checks.rc('get-text', '--extent', 'screen', '--add-wrap-markers')
        (root / 'wrap-restored.screen.txt').write_text(screen)
        lines = screen.splitlines()
        start = next(i for i, line in enumerate(lines) if line.startswith('WRAP_CHECK_'))
        assert lines[start] + lines[start+1] + lines[start+2] == 'WRAP_CHECK_' + 'W' * 70
        assert len(lines[start]) == len(lines[start+1]) == 40
        checks.results.append({'case': 'wrap-mode-restored', 'failures': []})
    finally:
        if checks:
            (root / 'results.json').write_text(json.dumps(checks.results, indent=2))
            checks.close()
    assert not any(item['failures'] for item in checks.results)

main()
