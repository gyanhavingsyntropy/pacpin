#!/usr/bin/env python3
"""Real orphan/cache transactions in disposable Garuda's Kitty terminal only."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

from verify_upgrade_ui_vm import Checks, run, serialized_verifier, transcript


class HousekeepingChecks(Checks):
    def suite(self):
        self.seed()
        self.publish(1)
        self.catalog(1)
        # Earlier suites used different isolated ALPM databases. Remove only
        # this suite's known paths in the disposable guest before reseeding.
        run("sudo", "-n", "rm", "-rf", "--", *[f"/opt/pacpin-ui-{name}" for name in ["backend", "spell", "viewer", "unused"]])
        self.cfg.write_text(self.cfg.read_text().replace("smart_orphans = false", "smart_orphans = true"))
        original_path = self.env["PATH"]
        helpers = self.root / "housekeeping-tools"
        helpers.mkdir()
        sudo = helpers / "sudo"
        normal_sudo = '''#!/bin/sh
case "$1:$2" in pacman:-Rn|pacman:-Scc|/usr/bin/pacman:-Scc|/usr/bin/rm:*|/usr/bin/rmdir:*) sleep .5;; esac
exec /usr/bin/sudo "$@"
'''
        sudo.write_text(normal_sudo)
        sudo.chmod(0o755)
        paru = helpers / "paru"
        paru.write_text('#!/bin/sh\ncase "$1" in -Sc|-Scc) sleep .5;; esac\nexec /usr/bin/paru "$@"\n')
        paru.chmod(0o755)
        self.env["PATH"] = f"{helpers}:{original_path}"
        archives = {}
        for name, dependencies in [("pacpin-ui-backend", []), ("pacpin-ui-spell", ["pacpin-ui-backend"]), ("pacpin-ui-viewer", ["pacpin-ui-spell"]), ("pacpin-ui-unused", [])]:
            build = self.root / name
            build.mkdir()
            (build / "fixture.install").write_text(f'''pre_remove() {{
  echo PACPIN_ORPHAN_ACTIVE_COMMIT
  if [ -f '{self.root}/pause-removal' ]; then sleep 10; else sleep .5; fi
}}
''')
            (build / "PKGBUILD").write_text(f'''pkgname={name}
pkgver=1
pkgrel=1
arch=('any')
pkgdesc='Housekeeping dependency fixture'
license=('MIT')
install=fixture.install
depends=({' '.join(repr(dep) for dep in dependencies)})
package() {{ install -Dm644 /etc/os-release "$pkgdir/opt/{name}/app"; }}
''')
            run("makepkg", "--nodeps", "--noconfirm", cwd=build)
            archives[name] = next(build.glob("*.pkg.tar.zst"))
        run("sudo", "-n", "pacman", "-U", "--asdeps", "--noconfirm", *map(str, archives.values()))
        parent = self.root / "db/local/pacpin-ui-retained-host-1-1"
        parent.mkdir()
        (parent / "desc").write_text("%NAME%\npacpin-ui-retained-host\n\n%VERSION%\n1-1\n\n%REASON%\n0\n\n%SIZE%\n1\n\n%OPTDEPENDS%\npacpin-ui-viewer: Document viewer\n\n")
        (parent / "files").write_text("%FILES%\n\n")
        try:
            self.case("orphan-review-without-downloads", ["orphans", "-c"], orphan_choice="n", orphans=False, cache=False, width=40)
            frames = json.loads((self.root / "orphan-review-without-downloads.frames.json").read_text())
            assert any("Review: Choosing orphan cleanup" in frame for frame in frames)
            assert not any("Pacman:" in frame for frame in frames)
            self.case("retained-viewer-keeps-transitive-dependencies", ["orphans", "-c", "--noconfirm"], orphans=True, cache=False, width=40)
            text = transcript(self.root / "retained-viewer-keeps-transitive-dependencies.raw.txt")
            assert "Keeping pacpin-ui-spell" in text and "Keeping pacpin-ui-backend" in text
            for name in ["pacpin-ui-viewer", "pacpin-ui-spell", "pacpin-ui-backend"]:
                assert run("pacman", "-Q", name).strip() == f"{name} 1-1"
            assert not Path("/opt/pacpin-ui-unused/app").exists()
            self.case("protected-selection-has-no-removal-stage", ["orphans", "-c", "--noconfirm"], orphans=False, cache=False)
            self.case("selected-parent-does-not-remove-unchecked-dependencies", ["orphans", "-c"], orphan_choice=["n", "end", "space"], orphans=True, width=40)
            assert not Path("/opt/pacpin-ui-viewer/app").exists()
            for name in ["pacpin-ui-spell", "pacpin-ui-backend"]:
                assert run("pacman", "-Q", name).strip() == f"{name} 1-1"
            run("sudo", "-n", "pacman", "-U", "--asdeps", "--noconfirm", str(archives["pacpin-ui-viewer"]))
            self.case("explicitly-selected-parent-and-dependencies-remove", ["orphans", "-c"], orphan_choice="a", orphans=True, width=40)
            assert not Path("/opt/pacpin-ui-viewer/app").exists()
            assert not (self.root / "db/db.lck").exists()
        finally:
            shutil.rmtree(parent)

        def install_unused():
            run("sudo", "-n", "pacman", "-U", "--asdeps", "--noconfirm", str(archives["pacpin-ui-unused"]))

        install_unused()
        self.publish(2)
        self.catalog(2)
        stale = self.root / "cache/download-old"
        stale.mkdir()
        (stale / "archive.part").write_text("partial download")
        os.utime(stale, (time.time() - 300, time.time() - 300))
        run("sudo", "-n", "chown", "-R", "nobody:nobody", str(stale))
        run("sudo", "-n", "chmod", "700", str(stale))
        # A bounded pause before the real cache cleaner allows inspection of
        # the VirtualBox console image as well as Kitty's rendered frames.
        sudo.write_text(normal_sudo.replace('case "$1:$2"', 'if [ "$1:$2" = /usr/bin/pacman:-Scc ]; then echo PACPIN_CACHE_VISUAL_CHECK; sleep 15; fi\ncase "$1:$2"'))
        self.case("native-source-orphans-cache-ordered-footer", ["-Su", "--autoremove", "--noconfirm", "--no-cache"], native=True, source=True, orphans=True, cache=True, completed_before_cache="Orphans", width=40)
        sudo.write_text(normal_sudo)
        assert not stale.exists()
        assert not any((self.root / "cache").iterdir())
        frames = json.loads((self.root / "native-source-orphans-cache-ordered-footer.frames.json").read_text())
        assert any("Sources: ✔" in frame and "Orphans:" in frame for frame in frames)
        text = transcript(self.root / "native-source-orphans-cache-ordered-footer.raw.txt")
        for phase in ["Checks:", "Records:", "Restart:", "Extracting source archive", "Releasing build scratch"]:
            assert phase in text, f"missing invocation phase: {phase}"
        assert not (self.root / "db/db.lck").exists()

        cache = self.root / "cache"
        outside = self.root / "must-preserve"
        outside.mkdir()
        (outside / "cookie").write_text("preserve unrelated files")
        link = cache / "download-link"
        link.symlink_to(outside, target_is_directory=True)
        (cache / "download-empty").mkdir()
        self.case("full-cache-removes-empty-dir-preserves-symlink-target", ["clean", "-Scc", "--noconfirm"], cache=True)
        assert (outside / "cookie").read_text() == "preserve unrelated files"
        assert not (cache / "download-empty").exists()
        link.unlink(missing_ok=True)
        recent = cache / "download-recent"
        recent.mkdir()
        (recent / "active.part").write_text("preserve recent download")
        self.case("recent-nonempty-download-is-preserved", ["clean", "-Scc", "--noconfirm"], cache=True, success=False, expected_error="Download directory cleanup exited")
        assert (recent / "active.part").read_text() == "preserve recent download"
        os.utime(recent, (time.time() - 300, time.time() - 300))
        self.case("partial-cache-preserves-nonempty-download", ["clean", "-Sc", "--noconfirm"], cache=True, success=False, expected_error="Download directory cleanup exited")
        assert (recent / "active.part").exists()
        self.case("aur-only-cache-preserves-native-downloads", ["clean", "-Scc", "--aur", "--noconfirm"], cache=True)
        assert (recent / "active.part").exists()
        self.case("full-cache-clears-stale-nonempty-download", ["clean", "-Scc", "--noconfirm"], cache=True, width=40)
        assert not recent.exists()

        flatpak = helpers / "flatpak"
        flatpak.write_text('#!/bin/sh\necho INTEGRATION_SCOPE_LEAK\nexit 23\n')
        flatpak.chmod(0o755)
        original_cfg = self.cfg.read_text()
        self.cfg.write_text(original_cfg.replace("integrations = false", "integrations = true").replace("flatpak = false", "flatpak = true"))
        try:
            for scope in ["--repo", "--aur"]:
                name = "scoped-cache-excludes-integrations-" + scope[2:]
                self.case(name, ["clean", "-Scc", scope, "--noconfirm"], cache=True)
                assert "INTEGRATION_SCOPE_LEAK" not in transcript(self.root / f"{name}.raw.txt")
                self.case(name + "-dry-run", ["clean", "-Scc", scope, "--dry-run"], cache=False)
                assert "Flatpak" not in transcript(self.root / f"{name}-dry-run.raw.txt")
            self.case("conflicting-cache-scopes-refused", ["clean", "-Scc", "--repo", "--aur", "--noconfirm"], cache=False, success=False, expected_error="cannot combine --repo with --aur")
        finally:
            self.cfg.write_text(original_cfg)
            flatpak.unlink()

        install_unused()
        pending = Path(self.env["XDG_STATE_HOME"]) / "pacpin/pending_orphan_cleanup.json"
        sudo.write_text('#!/bin/sh\nif [ "$1:$2" = pacman:-Rn ]; then echo "error: INJECTED_REMOVAL_FAILURE" >&2; sleep .5; exit 7; fi\nexec /usr/bin/sudo "$@"\n')
        self.case("failed-orphans-have-no-completion", ["orphans", "-c", "--noconfirm"], orphans=True, success=False, expected_error="INJECTED_REMOVAL_FAILURE")
        assert json.loads(pending.read_text()) == ["pacpin-ui-unused"]
        assert "Orphans: ✔" not in (self.root / "failed-orphans-have-no-completion.raw.txt").read_text()
        sudo.write_text('#!/bin/sh\nexec /usr/bin/sudo "$@"\n')
        (self.root / "pause-removal").touch()
        self.case("cancel-orphan-stage-preserves-pending", ["orphans", "-c", "--noconfirm"], orphans=True, success=False, cancel_manager="Orphans", cancel_marker="PACPIN_ORPHAN_ACTIVE_COMMIT")
        (self.root / "pause-removal").unlink()
        assert json.loads(pending.read_text()) == ["pacpin-ui-unused"]
        assert not (self.root / "db/db.lck").exists()
        install_unused()
        sudo.write_text('#!/bin/sh\nif [ "$1:$2" = pacman:-Rn ]; then sleep .5; fi\nexec /usr/bin/sudo "$@"\n')
        self.case("orphan-cancellation-next-run-removes", ["orphans", "-c", "--noconfirm"], orphans=True)
        assert json.loads(pending.read_text()) == []
        sudo.write_text('#!/bin/sh\ncase "$1:$2" in pacman:-Scc|/usr/bin/pacman:-Scc) sleep 10;; esac\nexec /usr/bin/sudo "$@"\n')
        self.case("cancel-cache-stage-restores-terminal", ["clean", "-Scc", "--noconfirm"], cache=True, success=False, cancel_manager="Cache")
        assert not (self.root / "db/db.lck").exists()
        self.env["PATH"] = original_path

        # Build failure is simulated, while independent Sources really installs.
        install_unused()
        self.seed()
        self.publish(1)
        self.catalog(2)
        hello = self.root / "db/local/hello-0.0.0-1"
        hello.mkdir()
        (hello / "desc").write_text("%NAME%\nhello\n\n%VERSION%\n0.0.0-1\n\n%REASON%\n0\n\n%SIZE%\n1\n\n%ARCH%\nx86_64\n\n")
        (hello / "files").write_text("%FILES%\n\n")
        paru = helpers / "paru"
        paru.write_text('#!/bin/sh\necho "==> Building fixture"\nsleep .5\necho "==> ERROR: A failure occurred in build()."\nexit 1\n')
        paru.chmod(0o755)
        original_cfg = self.cfg.read_text()
        try:
            self.cfg.write_text(original_cfg.replace('helper = "paru"', f'helper = "{paru}"').replace('[pins]', '[pins]\nhello = "aur"'))
            self.case("keep-going-source-success-skips-orphans-after-aur-failure", ["-Su", "--keep-going", "--autoremove", "--noconfirm"], aur=True, source=True, source_completed=True, success=False, expected_error="Orphan cleanup is skipped", orphans=False, cache=False)
            frames = json.loads((self.root / "keep-going-source-success-skips-orphans-after-aur-failure.frames.json").read_text())
            assert any("AUR: ✗ Operation failed" in frame and "Sources:" in frame for frame in frames)
            assert run("pacman", "-Q", "pacpin-ui-unused").strip() == "pacpin-ui-unused 1-1"
            assert json.loads(pending.read_text()) == ["pacpin-ui-unused"]
        finally:
            self.cfg.write_text(original_cfg)
            shutil.rmtree(hello)


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or os.geteuid() == 0:
        raise SystemExit("Run only as a regular user in the disposable VM")
    if "garuda" not in Path("/etc/os-release").read_text().lower() or run("systemd-detect-virt").strip() != "oracle" or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay":
        raise SystemExit("Requires the disposable VirtualBox Garuda live overlay")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        raise SystemExit("Use a fresh empty output directory")
    checks = None
    try:
        checks = HousekeepingChecks(args.binary, args.output, "unix:/tmp/pacpin-ui-kitty.sock", False, False, "all")
        checks.suite()
    finally:
        if checks is not None:
            (checks.root / "results.json").write_text(json.dumps(checks.results, indent=2) + "\n")
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(item["failures"] for item in checks.results):
        raise SystemExit("Housekeeping terminal checks failed")


if __name__ == "__main__":
    main()
