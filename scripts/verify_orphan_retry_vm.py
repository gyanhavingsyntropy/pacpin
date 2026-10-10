#!/usr/bin/env python3
"""Disposable Garuda/Kitty regression for orphan review after failed transactions.

Real makepkg/pacman fixtures; AUR failure is a controlled helper named paru.
Never run this on the host.
"""
import argparse
import json
import os
from pathlib import Path
import shutil

from verify_upgrade_ui_vm import Checks, run, serialized_verifier


class OrphanRetryChecks(Checks):
    def suite(self):
        self.seed()
        self.publish(1)
        self.catalog(1)
        self.cfg.write_text(self.cfg.read_text().replace("smart_orphans = false", "smart_orphans = true"))
        build = self.root / "orphan-build"
        build.mkdir()
        (build / "PKGBUILD").write_text('''pkgname=pacpin-ui-orphan
pkgver=1
pkgrel=1
arch=('any')
pkgdesc='Orphan retry fixture'
license=('MIT')
package() {
  install -d "$pkgdir/opt/pacpin-ui-orphan"
  printf 'orphan fixture\\n' > "$pkgdir/opt/pacpin-ui-orphan/app"
}
''')
        run("makepkg", "--force", "--nodeps", "--noconfirm", cwd=build)
        archive = next(build.glob("*.pkg.tar.*"))
        helpers = self.root / "controlled-aur"
        helpers.mkdir()
        helper = helpers / "paru"
        helper.write_text('#!/bin/sh\necho "==> Building controlled fixture"\nsleep 2\necho "==> ERROR: A failure occurred in build()."\nexit 1\n')
        helper.chmod(0o755)
        original_cfg = self.cfg.read_text()
        state = Path(self.env["XDG_STATE_HOME"]) / "pacpin"
        pending = state / "pending_orphan_cleanup.json"
        known = state / "known_orphans.json"

        def install_orphan():
            run("sudo", "-n", "pacman", "-U", "--asdeps", "--noconfirm", str(archive))

        def assert_installed():
            assert run("pacman", "-Q", "pacpin-ui-orphan").strip() == "pacpin-ui-orphan 1-1"

        def assert_pending():
            assert json.loads(pending.read_text()) == ["pacpin-ui-orphan"]

        def remember_inventory():
            run(str(self.binary), "orphans", env=self.env)
            assert "pacpin-ui-orphan" in json.loads(known.read_text())

        install_orphan()
        remember_inventory()
        assert not pending.exists(), "read-only listing must not acknowledge cleanup"
        hello = self.root / "db/local/hello-0.0.0-1"
        hello.mkdir()
        (hello / "desc").write_text("%NAME%\nhello\n\n%VERSION%\n0.0.0-1\n\n%REASON%\n0\n\n%SIZE%\n1\n\n%ARCH%\nx86_64\n\n")
        (hello / "files").write_text("%FILES%\n\n")
        try:
            self.cfg.write_text(original_cfg.replace('helper = "paru"', f'helper = "{helper}"').replace('[pins]', '[pins]\nhello = "aur"'))
            self.case("known-orphan-selected-before-aur-failure", ["-Su"], aur=True, confirm=True, orphan_choice="a", success=False, expected_error="pending cleanup will be offered again", width=40)
            assert_installed()
            assert_pending()
        finally:
            self.cfg.write_text(original_cfg)
            shutil.rmtree(hello)

        remember_inventory()
        assert_pending()
        self.case("noninteractive-run-preserves-pending", ["-Su", "--noconfirm"], expect_no_orphan_prompt=True)
        assert_installed()
        assert_pending()
        self.case("sources-scope-preserves-pending", ["-Su", "--sources-only", "--noconfirm"], expect_no_orphan_prompt=True)
        assert_pending()
        self.case("pending-cleanup-no-updates-retry", ["-Su"], orphan_choice="a", width=40)
        assert not Path("/opt/pacpin-ui-orphan/app").exists()
        assert json.loads(pending.read_text()) == []
        self.case("completed-cleanup-does-not-repeat", ["-Su"], expect_no_orphan_prompt=True)

        install_orphan()
        remember_inventory()
        pending.write_text('["pacpin-ui-orphan"]')
        fail_sudo = helpers / "sudo"
        fail_sudo.write_text('#!/bin/sh\nif [ "$1" = pacman ] && [ "$2" = -Rns ]; then echo "Injected orphan removal failure" >&2; exit 7; fi\nexec /usr/bin/sudo "$@"\n')
        fail_sudo.chmod(0o755)
        original_path = self.env["PATH"]
        self.env["PATH"] = f"{helpers}:{original_path}"
        try:
            self.case("orphan-removal-failure-retains-pending", ["-Su"], orphan_choice="a", success=False, expected_error="Injected orphan removal failure")
            assert_installed()
            assert_pending()
        finally:
            self.env["PATH"] = original_path
            fail_sudo.unlink()
        self.case("orphan-removal-failure-retry", ["-Su"], orphan_choice="a")
        assert not Path("/opt/pacpin-ui-orphan/app").exists()
        assert json.loads(pending.read_text()) == []

        install_orphan()
        remember_inventory()
        pending.write_text('["pacpin-ui-orphan"]')
        self.case("pending-cleanup-explicit-decline", ["-Su"], orphan_choice="n")
        assert_installed()
        assert json.loads(pending.read_text()) == []
        self.case("declined-cleanup-does-not-repeat", ["-Su"], expect_no_orphan_prompt=True)
        pending.unlink()
        self.case("legacy-known-inventory-fresh-review", ["-Su"], orphan_choice="n")
        assert json.loads(pending.read_text()) == []

        pending.unlink()
        pending.mkdir()
        try:
            self.case("pending-state-write-failure-stops-removal", ["-Su"], orphan_choice="a", success=False, expected_error="Cannot save pending orphan cleanup")
            assert_installed()
        finally:
            pending.rmdir()
        pending.write_text('["pacpin-ui-orphan"]')
        self.case("direct-cleanup-resolves-pending", ["orphans", "-c"], orphan_choice="a")
        assert json.loads(pending.read_text()) == []
        assert not Path("/opt/pacpin-ui-orphan/app").exists()


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
        checks = OrphanRetryChecks(args.binary, args.output, "unix:/tmp/pacpin-ui-kitty.sock", False, False, "all")
        checks.suite()
    finally:
        if checks is not None:
            (checks.root / "results.json").write_text(json.dumps(checks.results, indent=2) + "\n")
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(item["failures"] for item in checks.results):
        raise SystemExit("Orphan retry terminal checks failed")


if __name__ == "__main__":
    main()
