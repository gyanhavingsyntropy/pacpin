#!/usr/bin/env python3
"""Disposable Garuda discovery failures in the real terminal.

Native/Sources transactions are real. Flatpak/Nix/Pipx/custom adapters are
controlled fixtures. AUR RPC failure is injected through guest /etc/hosts;
both hosts and pacman configuration are restored, including on assertion failure.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import io
import tarfile

from verify_capabilities_vm import CapabilityChecks
from verify_upgrade_ui_vm import run, serialized_verifier


class DiscoveryChecks(CapabilityChecks):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        # This suite tests discovery/eligibility, not long download timing.
        # Retain a small read delay for observable source phases without the
        # UI suite's deliberately long delay on every archive inspection.
        source = self.root/'fast-read.c'
        source.write_text((self.root/'slow-read.c').read_text().replace('usleep(35000)', 'usleep(1000)'))
        preload = self.root/'fast-read.so'
        run('cc','-shared','-fPIC','-o',str(preload),str(source),'-ldl')
        self.env['LD_PRELOAD'] = str(preload)

    def failing(self, name, phase="check", malformed=False):
        helper = self.root / f"{name}-fixture"
        code = helper.read_text()
        insert = (f"if operation == {phase!r}:\n"
                  f"    Path({str(self.root / (name+'-discovery-call'))!r}).write_text(operation)\n"
                  + ("    print('{invalid JSON')\n    sys.exit(0)\n" if malformed else
                     "    print('injected discovery failure', file=sys.stderr)\n    sys.exit(7)\n"))
        if phase == 'refresh':
            insert += ("if operation == 'check':\n"
                       f"    Path({str(self.root / (name+'-check-after-refresh'))!r}).write_text(operation)\n")
        helper.write_text(code.replace("operation = sys.argv[1]\n", "operation = sys.argv[1]\n"+insert))
        if phase == "refresh":
            config = self.cfg.parent / 'managers.toml'
            config.write_text(config.read_text().replace(f"[managers.{name}]\n",
                              f"[managers.{name}]\nrefresh = {json.dumps([str(helper), 'refresh'])}\n"))

    def setup_case(self, source=True, names=()):
        self.cfg.write_text(self.original_cfg)
        self.configure(names=names)
        self.seed()
        self.publish(2)
        self.catalog(2 if source else 1)
        pending = self.root / "state/pacpin/pending_orphan_cleanup.json"
        pending.parent.mkdir(parents=True, exist_ok=True)
        pending.write_text('["pacpin-ui-preserved-orphan"]\n')
        self.cfg.write_text(self.cfg.read_text().replace('smart_orphans = false', 'smart_orphans = true'))
        (self.root/'native-hook-ran').unlink(missing_ok=True)
        return pending

    def checked(self, name, args, **kwargs):
        before = len(self.results)
        source_failed = 'Sources' in kwargs.get('skipped', ())
        if source_failed:
            kwargs['skipped'] = tuple(manager for manager in kwargs['skipped'] if manager != 'Sources')
            if kwargs.get('expected') == 'Sources skipped':
                kwargs['expected'] = 'ui-local/pacpin-ui-source skipped'
        self.check(name, args, **kwargs)
        assert len(self.results) == before+1
        frames = json.loads((self.root / f'{name}.frames.json').read_text())
        if source_failed:
            text = (self.root / f'{name}.raw.txt').read_text()
            if 'ui-local/pacpin-ui-source skipped' not in text:
                self.results[-1]['failures'].append('failed source package not identified')
            if kwargs.get('native_updated') and not kwargs.get('dry'):
                if not any('Sources: ⚠ 1 check(s) skipped' in frame and 'Pacman:' in frame for frame in frames):
                    self.results[-1]['failures'].append('source discovery warning missing during healthy native work')
        for manager in kwargs.get('skipped', ()):
            rows = [line for frame in frames for line in frame.splitlines()
                    if line.startswith(f'{manager}: ⚠ Skipped')]
            if any('(unsupported' in row for row in rows):
                self.results[-1]['failures'].append('discovery failure mislabeled as unsupported options')
            if kwargs.get('native_updated') and not kwargs.get('dry'):
                prefix = '(metadata refresh' if 'refresh-failure' in name else '(update'
                if not any(prefix in row for row in rows):
                    self.results[-1]['failures'].append(f'{manager} discovery reason missing from footer')
        pending = self.root / "state/pacpin/pending_orphan_cleanup.json"
        if pending.read_text() != '["pacpin-ui-preserved-orphan"]\n':
            self.results[-1]['failures'].append('pending orphan cleanup changed after incomplete discovery')
        expected_hook = kwargs.get('native_updated', False) and not kwargs.get('dry', False)
        if (self.root / 'native-hook-ran').exists() != expected_hook:
            self.results[-1]['failures'].append('verified native hook did not follow eligible work')
        (self.root / 'results.json').write_text(json.dumps(self.results, indent=2))

    def suite(self):
        self.direct_terminal = True
        self.job_control = True
        self.original_cfg = self.cfg.read_text()+f'''\n[hooks.discovery-test]
watch = ["pacpin-ui-native"]
action = "script"
command = ["/usr/bin/touch", "{self.root}/native-hook-ran"]
'''
        for keep in [False, True]:
            self.setup_case(source=False)
            (self.root / 'catalog.json').write_text('{invalid JSON')
            self.checked('sources-check-'+('keep' if keep else 'strict'),
                         ['-Su', '--no-cache', '--use-ram', '--noconfirm']+(['--keep-going'] if keep else []),
                         native_updated=keep, skipped=('Sources',) if keep else (),
                         expected='Sources skipped' if keep else 'Update check incomplete', status=2)
        self.setup_case(source=False)
        (self.root / 'catalog.json').write_text('{invalid JSON')
        self.checked('sources-only-all-failed', ['-Su', '--sources-only', '--keep-going', '--noconfirm'],
                     skipped=('Sources',), expected='No eligible package updates remain')
        self.setup_case(source=False)
        (self.root / 'catalog.json').write_text('{invalid JSON')
        self.checked('discovery-dry-run', ['-Su', '--keep-going', '--no-cache', '--use-ram', '--dry-run'],
                     native_updated=True, skipped=('Sources',), dry=True)
        for manager in ['flatpak', 'nix', 'pipx', 'custom']:
            self.setup_case(names=(manager,))
            self.failing(manager, malformed=manager=='custom')
            label = {'flatpak':'Flatpak','nix':'Nix','pipx':'Pipx'}.get(manager, manager)
            self.checked(manager+'-check-keeps-native-sources',
                         ['-Su', '--keep-going', '--no-cache', '--noconfirm'],
                         native_updated=True, source_updated=True, skipped=(label,), no_effect=(manager,), width=40)
        self.setup_case(names=('flatpak','nix'))
        self.failing('flatpak')
        self.checked('external-only-preserves-healthy-manager',
                     ['-Su', '--external-only', '--keep-going', '--no-cache', '--flatpak.review', '--noconfirm'],
                     skipped=('Flatpak',), no_effect=('flatpak',), expected='Upgrade incomplete')
        actions = json.loads((self.root/'nix-actions.json').read_text())
        if actions != ['upgrade','cache']:
            self.results[-1]['failures'].append('healthy Nix fixture did not upgrade and clean exactly once')
        self.setup_case(source=False, names=('flatpak',))
        self.failing('flatpak', phase='refresh')
        self.checked('refresh-failure-keeps-native', ['-Syu','--keep-going','--no-cache','--noconfirm'],
                     native_updated=True, skipped=('Flatpak',), no_effect=('flatpak',), expected='metadata refresh failed')
        if (self.root/'flatpak-discovery-call').read_text() != 'refresh':
            self.results[-1]['failures'].append('failed refresh manager was checked against stale metadata')
        if (self.root/'flatpak-check-after-refresh').exists():
            self.results[-1]['failures'].append('check executed after failed refresh')
        self.setup_case(source=False)
        (self.root / 'catalog.json').write_text('{invalid JSON')
        self.checked('read-only-check-stays-strict', ['check'], expected='Update check incomplete', status=1)
        # Only this disposable guest's resolver is redirected. No production
        # test-only RPC override is added to pacpin.
        original_hosts = Path('/etc/hosts').read_bytes()
        saved = self.root/'hosts.original'
        saved.write_bytes(original_hosts)
        blocked = self.root/'hosts.blocked'
        blocked.write_bytes(original_hosts+b'\n127.0.0.1 aur.archlinux.org\n')
        hello = self.root/'db/local/hello-0.0.0-1'
        try:
            run('sudo','-n','cp',str(blocked),'/etc/hosts')
            for keep in [False, True]:
                self.setup_case()
                self.cfg.write_text(self.cfg.read_text().replace('[pins]', '[pins]\nhello = "aur"'))
                self.cfg.write_text(self.cfg.read_text()+'''\n[hooks.aur-discovery-test]
watch = ["pacpin-ui-native"]
action = "rebuild-aur"
packages = ["hello"]
''')
                hello.mkdir(exist_ok=True)
                (hello/'desc').write_text('%NAME%\nhello\n\n%VERSION%\n0.0.0-1\n\n%REASON%\n0\n\n%SIZE%\n1\n\n%ARCH%\nx86_64\n\n')
                (hello/'files').write_text('%FILES%\n\n')
                self.checked('aur-check-'+('keep' if keep else 'strict'),
                             ['-Su','--no-cache','--noconfirm']+(['--keep-going','aur/hello'] if keep else []),
                             native_updated=keep, source_updated=keep, skipped=('AUR',) if keep else (),
                             expected='AUR skipped' if keep else 'Update check incomplete', status=2)
        finally:
            run('sudo','-n','cp',str(saved),'/etc/hosts')
            assert Path('/etc/hosts').read_bytes() == original_hosts
            if hello.exists(): shutil.rmtree(hello)


class ExtraChecks(DiscoveryChecks):
    def suite(self):
        self.direct_terminal = True
        self.job_control = True
        self.original_cfg = self.cfg.read_text()+f'''\n[hooks.discovery-test]
watch = ["pacpin-ui-native"]
action = "script"
command = ["/usr/bin/touch", "{self.root}/native-hook-ran"]
'''
        self.setup_case(source=False, names=('flatpak',))
        (self.root/'catalog.json').write_text('{invalid JSON')
        self.checked('sources-failure-keeps-healthy-flatpak', ['-Su','--keep-going','--no-cache','--noconfirm'],
                     native_updated=True, skipped=('Sources',))
        if json.loads((self.root/'flatpak-actions.json').read_text()) != ['upgrade','cache']:
            self.results[-1]['failures'].append('successful Flatpak fixture result was discarded or not cleaned')
        self.setup_case(source=False)
        (self.root/'catalog.json').write_text('{invalid JSON')
        self.checked('final-exact-flags-source-failure', ['-Syu','--keep-going','--no-cache','--use-ram','--noconfirm'],
                     native_updated=True, skipped=('Sources',))
        self.setup_case(source=False, names=('flatpak',))
        self.failing('flatpak', phase='refresh')
        self.checked('final-refresh-failure-keeps-native', ['-Syu','--keep-going','--no-cache','--noconfirm'],
                     native_updated=True, skipped=('Flatpak',), no_effect=('flatpak',), expected='metadata refresh failed')
        if (self.root/'flatpak-check-after-refresh').exists():
            self.results[-1]['failures'].append('check executed after failed refresh')
        self.setup_case(names=('flatpak','nix'))
        self.failing('flatpak')
        self.failing('nix')
        self.checked('multiple-discovery-failures', ['-Su','--keep-going','--no-cache','--noconfirm'],
                     native_updated=True, source_updated=True, skipped=('Flatpak','Nix'), no_effect=('flatpak','nix'), width=40)
        self.setup_case()
        database = self.root/'db/sync/pacpin-ui.db'
        patched = self.root/'dependency-refusal.db'
        with tarfile.open(database) as old, tarfile.open(patched, 'w:gz') as new:
            for member in old:
                if member.isfile():
                    body = old.extractfile(member).read()
                    if member.name.endswith('/desc'):
                        body += b'%DEPENDS%\nmissing-native-dependency>=1\n\n'
                    member.size = len(body)
                    new.addfile(member, io.BytesIO(body))
                else:
                    new.addfile(member)
        run('sudo','-n','cp',str(patched),str(database))
        self.checked('native-dependency-error-still-stops', ['-Su','--keep-going','--noconfirm'],
                     expected='requires missing-native-dependency>=1', status=1)
        self.setup_case(names=('flatpak',))
        self.failing('flatpak')
        marker = self.root/'flatpak-discovery-call'
        marker.unlink(missing_ok=True)
        self.checked('sources-scope-excludes-failing-external', ['-Su','--sources-only','--keep-going','--noconfirm'],
                     source_updated=True, success=True, no_effect=('flatpak',))
        if marker.exists():
            self.results[-1]['failures'].append('out-of-scope manager was checked')
        self.setup_case(names=('flatpak',))
        self.check('complete-discovery-keeps-normal-hooks', ['-Su','--keep-going','--no-cache','--noconfirm'],
                   native_updated=True, source_updated=True, success=True)
        if not (self.root/'native-hook-ran').exists():
            self.results[-1]['failures'].append('normal verified hook was suppressed after complete discovery')


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--socket', default='unix:/tmp/pacpin-ui-kitty.sock')
    parser.add_argument('--section', choices=['all','extra'], default='all')
    args = parser.parse_args()
    if (os.environ.get('PACPIN_DISPOSABLE_VM') != '1' or os.geteuid() == 0
        or 'ID=garuda' not in Path('/etc/os-release').read_text()
        or run('systemd-detect-virt').strip() != 'oracle'
        or run('findmnt','-n','-o','FSTYPE','/').strip() != 'overlay'):
        raise SystemExit('Only run as regular user in disposable Garuda live VM')
    args.output.mkdir(parents=True, exist_ok=False)
    checks = None
    try:
        checks_type = DiscoveryChecks if args.section == 'all' else ExtraChecks
        checks = checks_type(args.binary, args.output, args.socket, False, False, 'discovery')
        checks.suite()
    finally:
        if checks:
            (args.output/'results.json').write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output/'pacman.original.conf').exists():
            run('sudo','-n','cp',str(args.output/'pacman.original.conf'),'/etc/pacman.conf')
    if any(result['failures'] for result in checks.results):
        raise SystemExit('Discovery scenarios failed; inspect results.json')
    print(f'{len(checks.results)} discovery scenarios passed')


if __name__ == '__main__':
    main()
