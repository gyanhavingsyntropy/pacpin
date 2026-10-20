#!/usr/bin/env python3
"""Disposable Garuda-only, read-only CLI plans against private ALPM fixtures."""
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import time

assert os.getuid() != 0
assert 'garuda' in Path('/etc/os-release').read_text().lower()
assert subprocess.check_output(['systemd-detect-virt'], text=True).strip() == 'oracle'
lock = open('/tmp/pacpin-vm-verifier.lock', 'a')
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
root = Path(tempfile.mkdtemp(prefix='pacpin-native-lookup-', dir='/tmp'))
results = []

def desc(name, version, **fields):
    values = dict(NAME=name, VERSION=version, ARCH='x86_64', REASON='0',
                  FILENAME=f'{name}-{version}-x86_64.pkg.tar.zst', ISIZE='100', CSIZE='10')
    values.update(fields)
    return ''.join(f'%{key}%\n{value}\n\n' for key, value in values.items())

local = root / 'db/local'
local.mkdir(parents=True)
(root / 'db/sync').mkdir()
(local / 'ALPM_DB_VERSION').write_text('9\n')
for name, version in [('app', '1-1'), ('old-pkg', '3-1')]:
    entry = local / f'{name}-{version}'
    entry.mkdir()
    (entry / 'desc').write_text(desc(name, version))
    (entry / 'files').write_text('%FILES%\n\n')
repos = {
    'earlier': [('variant', '1-1', dict(PROVIDES='app', CONFLICTS='app')),
                ('component-variant', '1-1', dict(PROVIDES='component=2'))],
    'later': [('app', '2-1', dict(DEPENDS='component>=2')), ('component', '2-1', {}),
              ('new-pkg', '1-1', dict(REPLACES='old-pkg<2')),
              ('tool-provider', '1-1', dict(PROVIDES='virtual-app=1'))],
}
for repo, packages in repos.items():
    with tarfile.open(root / f'db/sync/{repo}.db', 'w:gz') as archive:
        for name, version, fields in packages:
            body = desc(name, version, **fields).encode()
            header = tarfile.TarInfo(f'{name}-{version}/desc')
            header.size = len(body)
            header.mode = 0o644
            archive.addfile(header, io.BytesIO(body))
cfg = root / 'config/pacpin'
cfg.mkdir(parents=True)
(cfg / 'config.toml').write_text('''repo_order = ["earlier", "later"]
[features]
shell_aliases = false
smart_orphans = false
integrations = false
vendor_stickiness = false
stability_delays = false
[options]
privilege_tool = "sudo"
helper = "paru"
''')
(root / 'cache').mkdir()
fixture_conf = root / 'pacman.fixture.conf'
fixture_conf.write_text(f'''[options]
RootDir = /
DBPath = {root}/db
CacheDir = {root}/cache
Architecture = x86_64
SigLevel = Never
[earlier]
Server = https://fixture.invalid/earlier
[later]
Server = https://fixture.invalid/later
''')
original = Path('/etc/pacman.conf').read_bytes()
(root / 'pacman.original.conf').write_bytes(original)
env = dict(os.environ, XDG_CONFIG_HOME=str(root / 'config'), XDG_STATE_HOME=str(root / 'state'))

def case(label, args, expected, absent=(), code=0):
    result = subprocess.run(['/tmp/pacpin', *args], env=env, text=True, capture_output=True, timeout=30)
    text = result.stdout + result.stderr
    (root / f'{label}.txt').write_text(text)
    assert result.returncode == code, (label, result.returncode, text)
    for value in expected:
        assert value in text, (label, value, text)
    for value in absent:
        assert value not in text, (label, value, text)
    results.append(dict(case=label, exit=result.returncode))

try:
    subprocess.run(['sudo', '-n', 'cp', str(fixture_conf), '/etc/pacman.conf'], check=True)
    case('upgrade', ['-Su', '--dry-run'], ['app', 'component'], ['variant'])
    case('upgrade-flags', ['-Su', '--dry-run', '--no-cache', '--use-ram', '--keep-going'],
         ['app', 'component'], ['variant'])
    case('check', ['check', '--repo'], ['app', 'component'], ['variant'])
    case('install', ['-S', '--dry-run', 'app'], ['app', 'component'], ['variant'])
    for command in ['run', 'try']:
        case(command, [command, '--dry-run', '--use-storage', 'app'],
             ['later/app-2-1-x86_64.pkg.tar.zst', 'later/component-2-1-x86_64.pkg.tar.zst'], ['variant'])
        case(command+'-qualified', [command, '--dry-run', 'earlier/app'],
             ['earlier/variant-1-1-x86_64.pkg.tar.zst'])
        case(command+'-virtual', [command, '--dry-run', 'virtual-app'],
             ['later/tool-provider-1-1-x86_64.pkg.tar.zst'])
    available = int(next(line.split()[1] for line in Path('/proc/meminfo').read_text().splitlines()
                         if line.startswith('MemAvailable:'))) * 1024
    if available < (2560+32)*1024*1024:
        case('ram-reserve-guard', ['run', '--dry-run', '--use-ram', 'app'],
             ['Insufficient scratch capacity for Ram policy', '2.5 GiB OS reserve'], code=1)
    else:
        case('ram-capacity', ['run', '--dry-run', '--use-ram', 'app'],
             ['later/app-2-1-x86_64.pkg.tar.zst'], ['variant'])
    case('virtual-install', ['-S', '--dry-run', 'virtual-app'], ['tool-provider'])
    case('conflict', ['-S', '--dry-run', 'earlier/app'],
         ['variant conflicts with app (app)'], code=1)
    case('replacement-nonmatch', ['-S', '--dry-run', 'new-pkg'], ['new-pkg'], ['old-pkg'])
    # Exercise the actual interactive footer/CLI transport in the guest terminal.
    tty_command = ' '.join([f'XDG_CONFIG_HOME={root}/config', f'XDG_STATE_HOME={root}/state',
                           '/tmp/pacpin -Su --dry-run --no-cache --use-ram --keep-going',
                           f'; echo LOOKUP_TERMINAL_EXIT:$?; touch {root}/terminal.done'])
    subprocess.run(['kitty', '@', '--to', 'unix:/tmp/pacpin-ui-kitty.sock', 'send-text', tty_command+'\n'], check=True)
    deadline = time.monotonic()+30
    while not (root / 'terminal.done').exists() and time.monotonic() < deadline:
        time.sleep(.2)
    assert (root / 'terminal.done').exists(), 'terminal run did not finish'
    screen = subprocess.check_output(['kitty', '@', '--to', 'unix:/tmp/pacpin-ui-kitty.sock',
                                      'get-text', '--extent', 'all'], text=True)
    (root / 'terminal.txt').write_text(screen)
    assert 'LOOKUP_TERMINAL_EXIT:0' in screen and 'variant' not in screen, screen
    results.append(dict(case='terminal-upgrade-flags', exit=0))
finally:
    subprocess.run(['sudo', '-n', 'cp', str(root / 'pacman.original.conf'), '/etc/pacman.conf'], check=True)
    assert Path('/etc/pacman.conf').read_bytes() == original
    (root / 'results.json').write_text(json.dumps(dict(binary_sha256=hashlib.sha256(Path('/tmp/pacpin').read_bytes()).hexdigest(),
                                                        cases=results, config_restored=True), indent=2))
print(root)
print(f'{len(results)} checks passed; guest pacman.conf restored')
