#!/usr/bin/env python3
"""Directory Sources and private build cleanup in the user-selected live guest.

Actual makepkg/pacman/Bubblewrap/Kitty and a guest-only NTFS3 loop filesystem.
External manager commands are named fixtures. Never run this on the host.
"""
import argparse
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile

from verify_upgrade_ui_vm import Checks, run, serialized_verifier, transcript


class DirectoryChecks(Checks):
    def close(self):
        # These names were installed only into this guest fixture's private DB.
        # Remove them while that config is still active, before restoring it.
        query = subprocess.run(["pacman", "-Qq", "cleanup-compiled", "pacpin-private-runtimedep", "hello"], text=True, capture_output=True)
        names = [name for name in query.stdout.splitlines() if name in ["cleanup-compiled", "pacpin-private-runtimedep", "hello"]]
        if names:
            run("sudo", "-n", "pacman", "-R", "--noconfirm", "--", *names)
        super().close()

    def case(self, name, args, *, directory=False, partial=False, **kwargs):
        before = self.fingerprint()
        with contextlib.redirect_stdout(io.StringIO()):
            super().case(name, args, **kwargs)
        result = self.results[-1]
        if directory:
            # The shared older verifier equates Sources with ALPM installation.
            # Directory Sources instead assert receipt identity and unchanged ALPM.
            result["failures"] = [error for error in result["failures"] if error != "Sources upgrade shown although it was never planned/invoked"]
            if before != self.fingerprint():
                result["failures"].append("directory operation changed the native package database")
        if partial:
            result["failures"] = [error for error in result["failures"] if error != "source failure reported as success"]
            if result["status"] != 2:
                result["failures"].append("incomplete discovery/capability plan did not return exit 2")
        print(json.dumps({key: result[key] for key in ["case", "status", "failures", "frames"]}), flush=True)
        if result["failures"]:
            raise AssertionError(f"{name}: {result['failures']}")
        return transcript(self.root / f"{name}.raw.txt")

    def fingerprint(self):
        return {str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in (self.root / "db/local").rglob("*") if path.is_file()}

    def config(self, destination, *, compile=False, extra=""):
        self.cfg.write_text(f'''repo_order = ["pacpin-ui"]
[features]
shell_aliases = false
smart_orphans = false
integrations = true
[options]
helper = "paru"
privilege_tool = "sudo"
[sources.directory]
kind = "local"
url = "{self.root}/directory-catalog.json"
[sources.directory.packages.directory-app]
asset = "source.tar"
format = "archive"
binaries = {{ directory-app = "bin/app" }}
install_dir = "{destination}"
{extra}
''')
        if compile:
            with self.cfg.open("a") as config:
                config.write(f'''[sources.compile]
kind="local"
url="{self.root}/compile-catalog.json"
[sources.compile.packages.cleanup-compiled]
asset="compile.tar"
format="source"
binaries={{ cleanup-compiled="app" }}
depends=["pacpin-private-runtimedep"]
[sources.compile.packages.cleanup-compiled.build]
output="out"
makedepends=["pacpin-private-builddep"]
scratch_mib=16
scratch_inodes=128
commands=[["mkdir","-p","{{output}}"],["pacpin-private-cc","{{source}}/main.c","-o","{{output}}/app"]]
''')

    def catalog_version(self, version, valid=True):
        (self.root / "directory-catalog.json").write_text(json.dumps({"version": 1, "packages": {"directory-app": [{"tag": f"v{version}.0.0", "assets": [{"name": "source.tar", "url": "source.tar", "sha256": self.sha if valid else "0" * 64}]}]}}))

    def receipt(self, destination):
        current = (destination / "current").resolve(strict=True)
        return json.loads((current / ".pacpin-directory.json").read_text())

    def basic(self, destination, prefix):
        self.config(destination)
        self.catalog_version(1)
        self.case(prefix + "-preview", ["-S", "directory/directory-app", "--dry-run", "--cleanup=temporary,cache", "--use-storage"], directory=True)
        assert not destination.exists()
        self.case(prefix + "-install", ["-S", "directory/directory-app", "--noconfirm", "--use-storage", "--cleanup=temporary,cache"], directory=True)
        assert self.receipt(destination)["version"] == "1.0.0-1"
        assert (destination / "current/bin/app").is_file()
        run(str(destination / "current/bin/app"))
        current = os.readlink(destination / "current")
        self.catalog_version(2)
        self.case(prefix + "-cancel", ["upgrade", "--sources-only", "--noconfirm"], directory=True, success=False, cancel=True)
        assert os.readlink(destination / "current") == current
        self.catalog_version(2, False)
        self.case(prefix + "-bad-digest", ["upgrade", "--sources-only", "--noconfirm"], directory=True, success=False, expected_error="SHA-256 mismatch")
        assert os.readlink(destination / "current") == current
        assert self.receipt(destination)["version"] == "1.0.0-1"
        self.catalog_version(2)
        self.case(prefix + "-check", ["check"], directory=True)
        self.case(prefix + "-update", ["upgrade", "--sources-only", "--noconfirm", "--use-storage", "--cleanup=temporary,cache"], directory=True, resize=True)
        assert self.receipt(destination)["version"] == "2.0.0-1"
        assert len(list((destination / "versions").iterdir())) == 1
        run(str(destination / "current/bin/app"))
        self.case(prefix + "-inventory", ["source", "list"], directory=True)
        self.case(prefix + "-capture", ["capture", "--dry-run"], directory=True)
        head, packages = self.cfg.read_text().split("[packages]", 1)
        assert "sources = []" in packages
        self.cfg.write_text(head + "[packages]" + packages.replace("sources = []", 'sources = ["directory/directory-app"]', 1))
        self.case(prefix + "-manifest-current", ["rebuild", "--dry-run", "--cleanup=temporary"], directory=True)
        self.case(prefix + "-removal-preview", ["-R", "directory/directory-app", "--dry-run"], directory=True)
        assert destination.exists()

    def suite(self):
        if self.section == "continuation":
            return self.continuation()
        self.publish(1)
        parent = Path("/var/tmp") / (self.root.name + " apps with spaces")
        parent.mkdir()
        destination = parent / "app"
        self.basic(destination, "linux")
        # Existing destinations are never implicitly overwritten.
        unmanaged = parent / "unmanaged"
        (unmanaged / "bin").mkdir(parents=True)
        shutil.copyfile("/usr/bin/true", unmanaged / "bin/app")
        os.chmod(unmanaged / "bin/app", 0o755)
        original = hashlib.sha256((unmanaged / "bin/app").read_bytes()).hexdigest()
        self.config(unmanaged)
        self.case("unmanaged-refusal", ["-S", "directory/directory-app", "--noconfirm"], directory=True, success=False, expected_error="already exists")
        adopted = parent / "adopted"
        self.config(adopted)
        self.case("adopt-preview", ["source", "adopt", "directory/directory-app", "--from", str(unmanaged), "--version", "1.0.0-1", "--dry-run"], directory=True)
        assert not adopted.exists()
        self.case("adopt-existing", ["source", "adopt", "directory/directory-app", "--from", str(unmanaged), "--version", "1.0.0-1", "--noconfirm"], directory=True)
        assert (adopted / "current").resolve() == unmanaged
        assert not (unmanaged / ".pacpin-directory.json").exists()
        self.catalog_version(2)
        self.case("adopt-update", ["upgrade", "--sources-only", "--noconfirm"], directory=True)
        assert self.receipt(adopted)["version"] == "2.0.0-1"
        self.case("adopt-remove", ["-R", "directory/directory-app", "--noconfirm"], directory=True)
        assert not adopted.exists()
        assert hashlib.sha256((unmanaged / "bin/app").read_bytes()).hexdigest() == original
        assert destination.exists()

        # Native filesystem behavior plus actual NTFS3 update publication.
        image = self.root / "ntfs.img"
        with image.open("wb") as file:
            file.truncate(96 * 1024 * 1024)
        self.ntfs = self.root / "ntfs"
        self.ntfs.mkdir()
        run("mkfs.ntfs", "--quick", "--force", str(image))
        run("sudo", "-n", "mount", "-t", "ntfs3", "-o", f"loop,uid={os.getuid()},gid={os.getgid()},acl", str(image), str(self.ntfs))
        try:
            self.basic(self.ntfs / "application", "ntfs")
            self.case("ntfs-remove", ["-R", "directory/directory-app", "--noconfirm"], directory=True)
        finally:
            self.config(destination)
            run("sudo", "-n", "umount", str(self.ntfs))
            image.unlink()

        # An explicit custom adapter must declare temporary cleanup support.
        script = self.root / "external-fixture"
        script.write_text(f'''#!/bin/bash
case "$1" in
 check) printf '[{{"id":"fixture.app","name":"fixture","repo":"fixture","version":"2"}}]';;
 upgrade) touch {self.root}/external-upgraded;;
 verify) test -f {self.root}/external-upgraded;;
 cleanup) touch {self.root}/external-cleaned;;
 fail) exit 9;;
esac
''')
        script.chmod(0o755)
        manager = self.cfg.parent / "managers.toml"
        manager.write_text(f'''[managers.fixture]
adapter="command"
enabled=true
check=["{script}","check"]
upgrade=["{script}","upgrade","{{id}}"]
verify=["{script}","verify","{{id}}"]
''')
        self.catalog_version(3)
        hook = self.root / "directory-hook"
        hook.write_text(f'#!/bin/sh\ntouch "{self.root}/directory-hook-ran"\n')
        hook.chmod(0o755)
        with self.cfg.open("a") as file:
            file.write(f'[hooks.directory]\nwatch=["directory-app"]\naction="script"\ncommand=["{hook}"]\n')
        self.case("unsupported-cleanup-strict", ["upgrade", "--cleanup=temporary", "--noconfirm"], directory=True, success=False, expected_error="no --cleanup=temporary contract")
        assert self.receipt(destination)["version"] == "2.0.0-1"
        self.case("unsupported-cleanup-keep-going", ["upgrade", "--cleanup=temporary", "--keep-going", "--noconfirm"], directory=True, partial=True, success=False, expected_error="Upgrade incomplete")
        assert self.receipt(destination)["version"] == "3.0.0-1"
        assert (self.root / "directory-hook-ran").exists()
        assert not (self.root / "external-upgraded").exists()
        with manager.open("a") as file:
            file.write(f'temporary_cleanup=["{script}","cleanup"]\n')
        self.case("external-cleanup-success", ["upgrade", "--external-only", "--cleanup=temporary", "--noconfirm"])
        assert (self.root / "external-cleaned").exists()
        manager.write_text(manager.read_text().replace('"cleanup"]', '"fail"]'))
        self.case("external-cleanup-failure", ["upgrade", "--external-only", "--cleanup=temporary", "--noconfirm"], success=False, expected_error="exited with errors")
        manager.unlink()

        self.private_builds(destination)

    def private_builds(self, destination):
        shutil.copytree("/var/lib/pacman/local", self.root / "db/local", dirs_exist_ok=True)
        self.env["LD_PRELOAD"] = ""
        self.env["PACPIN_UI_SLOW_ARTIFACT"] = ""
        build = self.root / "private-builddep"
        build.mkdir()
        (build / "cc").write_text('#!/bin/sh\nexec /usr/bin/cc "$@"\n')
        (build / "PKGBUILD").write_text('''pkgname=pacpin-private-builddep
pkgver=1
pkgrel=1
arch=('any')
options=('!strip' '!debug')
package() { install -Dm755 "$startdir/cc" "$pkgdir/usr/bin/pacpin-private-cc"; }
''')
        run("makepkg", "--nodeps", "--noconfirm", "--nocheck", cwd=build)
        archive = next(build.glob("*.pkg.tar.zst"))
        shutil.copyfile(archive, self.repo / archive.name)
        run("repo-add", str(self.repo / "pacpin-ui.db.tar.gz"), str(self.repo / archive.name))
        runtime = self.root / "private-runtime"
        runtime.mkdir()
        (runtime / "PKGBUILD").write_text("pkgname=pacpin-private-runtimedep\npkgver=1\npkgrel=1\narch=('any')\noptions=('!strip' '!debug')\npackage() { install -Dm644 /dev/null \"$pkgdir/usr/share/pacpin-private-runtimedep\"; }\n")
        run("makepkg", "--nodeps", "--noconfirm", cwd=runtime)
        runtime_archive = next(runtime.glob("*.pkg.tar.zst"))
        shutil.copyfile(runtime_archive, self.repo / runtime_archive.name)
        run("repo-add", str(self.repo / "pacpin-ui.db.tar.gz"), str(self.repo / runtime_archive.name))
        run("sudo", "-n", "pacman", "-Sy", "--noconfirm")
        code = b'#include <stdio.h>\nint main(void) { puts("PRIVATE_BUILD_OK"); return 0; }\n'
        with tarfile.open(self.root / "compile.tar", "w") as tar:
            header = tarfile.TarInfo("main.c"); header.size = len(code); header.mode = 0o644
            tar.addfile(header, io.BytesIO(code))
        digest = hashlib.sha256((self.root / "compile.tar").read_bytes()).hexdigest()
        (self.root / "compile-catalog.json").write_text(json.dumps({"version": 1, "packages": {"cleanup-compiled": [{"tag": "v1.0.0", "assets": [{"name": "compile.tar", "url": "compile.tar", "sha256": digest}]}]}}))
        self.config(destination, compile=True)
        before = self.fingerprint()
        self.case("private-source-run", ["run", "compile/cleanup-compiled", "--cleanup=temporary", "--use-storage", "--noconfirm"])
        assert self.fingerprint() == before
        assert not Path("/usr/bin/pacpin-private-cc").exists()
        assert subprocess.run(["pacman", "-Q", "pacpin-private-runtimedep"], capture_output=True).returncode != 0
        self.case("private-source-install", ["-S", "compile/cleanup-compiled", "--cleanup=temporary", "--use-storage", "--noconfirm"], source=True, source_package="cleanup-compiled", source_version="1.0.0-1")
        assert "cleanup-compiled 1.0.0-1" in run("pacman", "-Q", "cleanup-compiled")
        assert subprocess.run(["pacman", "-Q", "pacpin-private-builddep"], capture_output=True).returncode != 0
        assert not Path("/usr/bin/pacpin-private-cc").exists()
        assert run("pacman", "-Qqd", "pacpin-private-runtimedep").strip() == "pacpin-private-runtimedep"
        self.case("real-aur-private-install", ["-S", "aur/hello", "--cleanup=temporary", "--use-storage"], aur=True, timeout=360, confirm=True, prepare_confirm=True)
        assert not list(Path("/var/tmp").glob("pacpin-build-*-pid*"))
        self.case("native-policy-no-build", ["-S", "pacpin-ui/pacpin-ui-native", "--cleanup=temporary", "--dry-run"])

    def continuation(self):
        """Real makepkg failure; recipe fetch is an explicitly named fixture."""
        self.publish(1)
        parent = Path("/var/tmp") / (self.root.name + " apps")
        parent.mkdir()
        destination = parent / "app"
        self.config(destination)
        self.catalog_version(1)
        self.case("continuation-seed-directory", ["-S", "directory/directory-app", "--noconfirm"], directory=True)
        build = self.root / "base-devel"
        build.mkdir()
        (build / "PKGBUILD").write_text("pkgname=base-devel\npkgver=1\npkgrel=1\narch=('any')\noptions=('!strip' '!debug')\npackage() { :; }\n")
        run("makepkg", "--nodeps", "--noconfirm", cwd=build)
        archive = next(build.glob("*.pkg.tar.zst"))
        shutil.copyfile(archive, self.repo / archive.name)
        run("repo-add", str(self.repo / "pacpin-ui.db.tar.gz"), str(self.repo / archive.name))
        run("sudo", "-n", "pacman", "-Sy", "--noconfirm")
        fixture = self.root / "aur-fixture"
        fixture.mkdir()
        (fixture / "PKGBUILD").write_text("pkgname=hello\npkgver=2.12.2\npkgrel=1\narch=('x86_64')\nbuild() { return 1; }\npackage() { :; }\n")
        (fixture / ".SRCINFO").write_text("pkgbase = hello\npkgver = 2.12.2\npkgrel = 1\narch = x86_64\npkgname = hello\n")
        installed = Path("/usr/local/lib") / ("pacpin-directory-aur-fixture-" + str(os.getpid()))
        assert not installed.exists()
        (fixture / "paru").write_text(f'#!/bin/sh\nset -eu\ntest "$1" = -G\nmkdir hello\ncp "{installed}/PKGBUILD" hello/PKGBUILD\ncp "{installed}/.SRCINFO" hello/.SRCINFO\n')
        (fixture / "paru").chmod(0o755)
        run("sudo", "-n", "cp", "-r", str(fixture), str(installed))
        try:
            text = self.cfg.read_text().replace('helper = "paru"', f'helper = "{installed}/paru"')
            self.cfg.write_text(text)
            self.catalog_version(2)
            before = self.fingerprint()
            self.case("private-aur-build-failure-continues-directory", ["upgrade", "aur/hello", "--cleanup=temporary", "--keep-going", "--noconfirm", "--use-storage"], directory=True, success=False, aur=True, source_completed=True, expected_error="The AUR stage failed", timeout=180)
            assert self.receipt(destination)["version"] == "2.0.0-1"
            assert before == self.fingerprint()
            assert subprocess.run(["pacman", "-Q", "base-devel"], capture_output=True).returncode != 0
            assert not list(Path("/var/tmp").glob("pacpin-build-*-pid*"))
        finally:
            run("sudo", "-n", "rm", "-rf", "--", str(installed))
        for index, args in enumerate([
            ["-Sy", "--cleanup", "temporary"],
            ["-S", "--downloadonly", "absent", "--cleanup=temporary"],
            ["-Syu", "--downloadonly", "--cleanup=cache"],
            ["-S", "--downloadonly", "absent", "--no-cache"],
        ]):
            self.case(f"irrelevant-policy-{index}", args, success=False)
            assert self.results[-1]["status"] == 2


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--socket", default="unix:/tmp/pacpin-ui-kitty.sock")
    parser.add_argument("--section", choices=["all", "continuation"], default="all")
    args = parser.parse_args()
    if os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or os.geteuid() == 0 or 'ID=garuda' not in Path("/etc/os-release").read_text() or run("systemd-detect-virt").strip() != "oracle" or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay":
        raise SystemExit("Requires the user-selected disposable Garuda VirtualBox live guest as regular user")
    args.output.mkdir(exist_ok=False)
    checks = None
    try:
        checks = DirectoryChecks(args.binary, args.output, args.socket, False, False, args.section)
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")


if __name__ == "__main__":
    main()
