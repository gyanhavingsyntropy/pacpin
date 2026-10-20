#!/usr/bin/env python3
"""Real Garuda terminal: concurrent source checks and per-package failures.

Native/Sources use real pacman/makepkg. Flatpak is a controlled adapter. A tiny
read interposer delays local catalog reads, recording overlap without changing
production HTTP/TLS policy or depending on the public Internet's timing.
"""
import argparse
import json
import os
from pathlib import Path

from verify_discovery_vm import DiscoveryChecks
from verify_upgrade_ui_vm import run, serialized_verifier


class SourceChecks(DiscoveryChecks):
    packages = ('pacpin-ui-source', 'pacpin-ui-sibling', 'pacpin-ui-broken', 'pacpin-ui-other')

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        # Prior private ALPM databases do not own files in this fresh fixture.
        # Delete only this suite's three known disposable guest app trees and
        # executable links; preserve every unrelated guest path.
        self.cleanup_extra()

    def cleanup_extra(self):
        run('sudo', '-n', 'rm', '-rf', '--',
            *[directory+name for name in self.packages[1:] for directory in ['/usr/bin/', '/opt/']])

    def close(self):
        try:
            super().close()
        finally:
            self.cleanup_extra()

    def prepare_sources(self, broken=True, names=()):
        (self.root/'flatpak-refresh-timing.json').unlink(missing_ok=True)
        # Seeding is setup, not a timing scenario. Delay only the catalog reads
        # during the command under test, never repeated archive inspections.
        self.env['LD_PRELOAD'] = ''
        self.cfg.write_text(self.original_cfg)
        self.configure(names=names)
        text = self.cfg.read_text().replace('pacpin-ui-source = "ui-local"',
            'pacpin-ui-source = "ui-local"\npacpin-ui-sibling = "ui-local"\n'
            'pacpin-ui-broken = "ui-local"\npacpin-ui-other = "ui-other"')
        for name in self.packages[1:]:
            alias = 'ui-other' if name.endswith('other') else 'ui-local'
            if alias == 'ui-other':
                text += f'\n[sources.ui-other]\nkind = "local"\nurl = "{self.root}/catalog-other.json"\n'
            text += f'''\n[sources.{alias}.packages.{name}]
asset = "source.tar"
format = "archive"
depends = []
binaries = {{ {name} = "bin/app" }}
'''
        self.cfg.write_text(text)
        self.seed()
        self.catalogs(1, False)
        for name in self.packages[1:]:
            alias = 'ui-other' if name.endswith('other') else 'ui-local'
            run(str(self.binary), '-S', f'{alias}/{name}', '--noconfirm', env=self.env)
        self.publish(2)
        self.catalogs(2, broken)

    def catalogs(self, version, broken):
        records = {}
        for name in self.packages:
            asset = 'missing.tar' if broken and name.endswith('broken') else 'source.tar'
            records[name] = [{'tag': f'v{version}.0.0', 'assets': [
                {'name': asset, 'url': 'source.tar', 'sha256': self.sha}]}]
        for filename in ['catalog.json', 'catalog-other.json']:
            (self.root / filename).write_text(json.dumps({'version': 1, 'packages': records}))

    def slow_checks(self):
        events = self.root / 'discovery-events.txt'
        events.unlink(missing_ok=True)
        source = self.root / 'catalog-delay.c'
        source.write_text(r'''
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
static ssize_t (*real_read)(int, void *, size_t);
__attribute__((constructor)) static void init(void) { real_read = dlsym(RTLD_NEXT, "read"); }
static void event(const char *path, const char *phase) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    int fd = open("EVENT_FILE", O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
    if (fd < 0) return;
    char line[1024];
    int n = snprintf(line, sizeof line, "%ld.%09ld %ld %s %s\n", now.tv_sec, now.tv_nsec,
                     syscall(SYS_gettid), phase, path);
    if (n > 0 && n < sizeof line) { ssize_t ignored = write(fd, line, n); (void) ignored; }
    close(fd);
}
ssize_t read(int fd, void *buffer, size_t size) {
    char descriptor[64], path[4096];
    snprintf(descriptor, sizeof descriptor, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(descriptor, path, sizeof path - 1);
    int slow = 0;
    if (n > 0) {
        path[n] = 0;
        slow = (!strcmp(path, "CATALOG_ONE") || !strcmp(path, "CATALOG_TWO")) && lseek(fd, 0, SEEK_CUR) == 0;
    }
    if (slow) { event(path, "start"); usleep(800000); }
    ssize_t result = real_read(fd, buffer, size);
    if (slow) event(path, "end");
    return result;
}
'''.replace('EVENT_FILE', str(events)).replace('CATALOG_ONE', str(self.root/'catalog.json'))
    .replace('CATALOG_TWO', str(self.root/'catalog-other.json')))
        preload = self.root / 'catalog-delay.so'
        run('cc', '-shared', '-fPIC', '-o', str(preload), str(source), '-ldl')
        self.env['LD_PRELOAD'] = str(preload)
        return events

    def assert_case(self, name, updated, status, timing=False, native=False):
        result = self.results[-1]
        failures = result['failures']
        if result['status'] != status:
            failures.append(f'expected status {status}, got {result["status"]}')
        for package in self.packages:
            version = '2.0.0-1' if updated and not package.endswith('broken') else '1.0.0-1'
            if run('pacman', '-Q', package).strip() != f'{package} {version}':
                failures.append(f'{package} did not follow its own discovery result')
        text = (self.root / f'{name}.raw.txt').read_text()
        frames = json.loads((self.root / f'{name}.frames.json').read_text())
        native_version = '2.0.0-1' if native else '1.0.0-1'
        if run('pacman', '-Q', 'pacpin-ui-native').strip() != 'pacpin-ui-native '+native_version:
            failures.append('native result differs from eligible plan')
        if native:
            scrollback = (self.root / f'{name}.screen.txt').read_text()
            if not all(f'PACPIN_NATIVE_HISTORY_{index:02}' in scrollback for index in range(1, 41)):
                failures.append('native history overwritten by later Sources output')
        if status == 2 and 'ui-local/pacpin-ui-broken' not in text:
            failures.append('failed package identity missing')
        if updated:
            if not any('Sources:' in frame and 'check(s) skipped' in frame for frame in frames):
                failures.append('partial discovery missing from Sources footer')
            if 'Sources skipped (--keep-going' in text:
                failures.append('entire Sources adapter was skipped')
        if 'Waiting for background source download' in text or '[Background]' in text:
            failures.append('implementation wording remains in user output')
        if timing:
            entries = [line.split(maxsplit=3) for line in (self.root/'discovery-events.txt').read_text().splitlines()]
            active, peak = 0, 0
            for timestamp, tid, phase, path in sorted(entries, key=lambda row: float(row[0])):
                active += 1 if phase == 'start' else -1
                peak = max(peak, active)
            if peak < 3:
                failures.append(f'source checks did not overlap: peak {peak}')
            result['concurrent_catalog_reads'] = peak
            refresh = self.root / 'flatpak-refresh-timing.json'
            if refresh.exists():
                start, end = json.loads(refresh.read_text())
                reads = [float(row[0]) for row in entries if row[2] == 'start']
                # Sources can finish before the refresh helper even starts its
                # delay (process/interpreter startup is part of refreshing).
                # Starting before refresh completion proves there is no barrier.
                if not any(timestamp < end for timestamp in reads):
                    failures.append('Sources waited for Flatpak refresh')
                result['sources_started_before_refresh_finished'] = min(reads) < end
                if not any('Flatpak: Refreshing metadata' in frame and 'Sources: Checking updates' in frame for frame in frames):
                    failures.append('concurrent discovery phases absent from footer')
                if json.loads((self.root/'flatpak-actions.json').read_text()) != ['upgrade', 'cache']:
                    failures.append('healthy Flatpak fixture did not upgrade and clean exactly once')
        (self.root/'results.json').write_text(json.dumps(self.results, indent=2))

    def suite(self):
        self.direct_terminal = True
        self.job_control = True
        self.original_cfg = self.cfg.read_text()
        if self.section != 'refresh':
            self.prepare_sources()
            self.slow_checks()
            self.case('source-strict', ['-Su', '--noconfirm'], success=False,
                      expected_error='Update check incomplete')
            self.assert_case('source-strict', False, 2, timing=True)
        cases = [
            ('same-and-other-alias-keep', ['-Su','--keep-going','--no-cache','--noconfirm'], True, True, False),
            ('sources-only-keep', ['-Su','--sources-only','--keep-going','--no-cache','--noconfirm'], True, False, False),
            ('source-partial-dry-run', ['-Su','--sources-only','--keep-going','--use-ram','--no-cache','--dry-run'], False, False, False),
            ('sources-current-plus-failure', ['-Su','--sources-only','--keep-going','--noconfirm'], False, False, True),
            ('sources-overlap-refresh', ['-Syu','--keep-going','--no-cache','--noconfirm'], True, True, False),
        ]
        if self.section == 'refresh':
            cases = cases[-1:]
        for name, args, updated, native, current in cases:
            self.prepare_sources(names=('flatpak',) if name.endswith('refresh') else ())
            if current:
                self.catalogs(1, True)
            if name.endswith('refresh'):
                manager_config = self.cfg.parent/'managers.toml'
                helper = self.root/'flatpak-fixture'
                timing = self.root/'flatpak-refresh-timing.json'
                code = helper.read_text().replace("operation = sys.argv[1]\n", "operation = sys.argv[1]\n" +
                    f"if operation == 'refresh':\n    start = time.monotonic()\n    time.sleep(2.5)\n    Path({str(timing)!r}).write_text(json.dumps([start, time.monotonic()]))\n    sys.exit(0)\n")
                helper.write_text(code)
                manager_config.write_text(manager_config.read_text().replace('[managers.flatpak]\n',
                    '[managers.flatpak]\nrefresh = '+json.dumps([str(helper), 'refresh'])+'\n'))
            self.slow_checks()
            self.case(name, args, native=native, source=updated, success=False,
                      source_completed=updated, expected_error='Upgrade incomplete:', width=80)
            self.assert_case(name, updated, 2, timing=True, native=native)
        if self.section != 'refresh':
            self.prepare_sources()
            self.slow_checks()
            self.case('strict-read-only-check', ['check'], success=False, expected_error='Update check incomplete')
            self.assert_case('strict-read-only-check', False, 1, timing=True)


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--socket', default='unix:/tmp/pacpin-ui-kitty.sock')
    parser.add_argument('--section', choices=['all', 'refresh'], default='all')
    args = parser.parse_args()
    if (os.environ.get('PACPIN_DISPOSABLE_VM') != '1' or os.geteuid() == 0
        or 'ID=garuda' not in Path('/etc/os-release').read_text()
        or run('systemd-detect-virt').strip() != 'oracle'
        or run('findmnt','-n','-o','FSTYPE','/').strip() != 'overlay'):
        raise SystemExit('Only run as regular user in disposable Garuda live VM')
    args.output.mkdir(parents=True, exist_ok=False)
    checks = None
    try:
        checks = SourceChecks(args.binary, args.output, args.socket, False, False, args.section)
        checks.suite()
    finally:
        if checks:
            (args.output/'results.json').write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output/'pacman.original.conf').exists():
            run('sudo','-n','cp',str(args.output/'pacman.original.conf'),'/etc/pacman.conf')
    if any(result['failures'] for result in checks.results):
        raise SystemExit('Source discovery scenarios failed; inspect results.json')
    print(f'{len(checks.results)} source discovery scenarios passed')


if __name__ == '__main__':
    main()
