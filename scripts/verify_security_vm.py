#!/usr/bin/env python3
"""Archive and cache regressions, ONLY in disposable Garuda's Kitty terminal."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import tarfile

from verify_upgrade_ui_vm import Checks, run, serialized_verifier


class SecurityChecks(Checks):
    def archive(self, escape):
        with tarfile.open(self.artifact, "w") as archive:
            archive.add("/usr/bin/true", arcname="bin/app")
            directory = tarfile.TarInfo("dir")
            directory.type = tarfile.DIRTYPE
            directory.mode = 0o755
            archive.addfile(directory)
            for name, target in [
                ("dir/alias", "."),
                ("link", "dir/alias/../../outside" if escape else "dir/alias/../bin/app"),
            ]:
                link = tarfile.TarInfo(name)
                link.type = tarfile.SYMTYPE
                link.linkname = target
                link.mode = 0o777
                archive.addfile(link)
            padding = os.urandom(1024 * 1024)
            entry = tarfile.TarInfo("padding")
            entry.size = len(padding)
            entry.mode = 0o644
            archive.addfile(entry, io.BytesIO(padding))
        self.sha = hashlib.sha256(self.artifact.read_bytes()).hexdigest()
        self.catalog(2)

    def suite(self):
        self.seed()
        self.publish(1)
        self.archive(escape=True)
        self.case("reject-intermediate-symlink-escape", ["-Su", "--sources-only", "--noconfirm"],
                  source=True, success=False, expected_error="outside sandbox root")
        if run("pacman", "-Q", "pacpin-ui-source").strip() != "pacpin-ui-source 1.0.0-1":
            raise AssertionError("Rejected source archive changed the installed package")
        self.archive(escape=False)
        self.case("allow-safe-intermediate-symlink", ["-Su", "--sources-only", "--noconfirm"], source=True)
        installed = Path("/opt/pacpin-ui-source/link")
        if installed.resolve() != Path("/opt/pacpin-ui-source/bin/app"):
            raise AssertionError("Safe installed link does not resolve to its declared payload")

        # Controlled Flatpak discovery only; no real app installation/removal.
        helpers = self.root / "security-helpers"
        helpers.mkdir()
        calls = self.root / "flatpak-remote-calls"
        flatpak = helpers / "flatpak"
        flatpak.write_text(f'''#!/bin/sh
case "$1" in
  --version) echo 'Flatpak security fixture';;
  list) echo user;;
  remote-ls) echo checked >> '{calls}'; printf 'Fixture\\tapp/org.pacpin.SecurityFixture/x86_64/stable\\t2.0\\tflathub\\t{'b' * 64}\\n';;
  *) exit 91;;
esac
''')
        flatpak.chmod(0o755)
        home = self.root / "cache-home"
        home.mkdir()
        self.env["HOME"] = str(home)
        self.env["PATH"] = f"{helpers}:{self.env['PATH']}"
        # Source installation serializes defaults, including [integrations].
        # Modify that existing table instead of appending a duplicate table.
        self.cfg.write_text(self.cfg.read_text().replace("integrations = false", "integrations = true")
                            .replace("flatpak = false", "flatpak = true"))
        cache = home / ".cache/pacpin/flatpak_updates.json"
        cache.parent.mkdir(parents=True)
        sentinel = home / "keep"
        sentinel.write_text("preserve this file")
        cache.symlink_to(sentinel)
        self.case("flatpak-cache-symlink-fallback", ["check"])
        if sentinel.read_text() != "preserve this file" or cache.is_symlink():
            raise AssertionError("Cache write followed a symlink")
        if cache.stat().st_mode & 0o777 != 0o600:
            raise AssertionError("Cache is not private")
        if json.loads(cache.read_text())[0]["id"] != "app/org.pacpin.SecurityFixture/x86_64/stable":
            raise AssertionError("Fresh discovery was not cached")
        self.case("flatpak-cache-hit", ["check"])
        if calls.read_text().splitlines() != ["checked"]:
            raise AssertionError("Fresh cache unnecessarily repeated remote discovery")
        if list(cache.parent.glob(".flatpak-updates-*.tmp")):
            raise AssertionError("Cache write left temporary files behind")


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--socket", default="unix:/tmp/pacpin-ui-kitty.sock")
    args = parser.parse_args()
    if (os.environ.get("PACPIN_DISPOSABLE_VM") != "1"
            or "ID=garuda" not in Path("/etc/os-release").read_text()
            or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay"
            or run("systemd-detect-virt").strip() != "oracle" or os.geteuid() == 0):
        raise SystemExit("Run only as the regular user in disposable Garuda")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        raise SystemExit("Use a fresh empty evidence directory")
    checks = None
    try:
        checks = SecurityChecks(args.binary, args.output, args.socket, False, False, "all")
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Security CLI regressions failed; inspect the evidence")


if __name__ == "__main__":
    main()
