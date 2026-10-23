#!/usr/bin/env python3
"""Temporary package builds/exports in the selected disposable Garuda guest.

Uses actual makepkg/Bubblewrap/ALPM and Kitty screen/scrollback captures. Only the
hello case accesses real AUR; the custom exporter is an explicitly named fixture.
Never run this on the host. Original guest pacman configuration is restored.
"""
import argparse
import contextlib
import hashlib
import io
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tarfile

from verify_upgrade_ui_vm import Checks, run, serialized_verifier, transcript


class EphemeralChecks(Checks):
    def case(self, name, args, **kwargs):
        with contextlib.redirect_stdout(io.StringIO()):
            super().case(name, args, **kwargs)
        result = self.results[-1]
        frames = json.loads((self.root / f"{name}.frames.json").read_text())
        if any("App: Verifying packages" in frame or "Build complete; awaiting installation" in frame for frame in frames):
            result["failures"].append("temporary preparation was described as a package transaction")
        if re.search(r"(?:^|\n)App:", self.rc("get-text", "--extent", "screen")):
            result["failures"].append("preparation footer remained after app execution")
        print(json.dumps({key: result[key] for key in ["case", "status", "failures", "frames"]}), flush=True)
        if result["failures"]:
            raise AssertionError(f"{name}: {result['failures']}; inspect its raw and screen logs")

    def suite(self):
        self.env["LD_PRELOAD"] = ""
        self.env["PACPIN_UI_SLOW_ARTIFACT"] = ""
        if self.section == "git":
            return self.git_case()
        # Reuse installed dependency metadata without installing fixture packages.
        shutil.copytree("/var/lib/pacman/local", self.root / "db/local", dirs_exist_ok=True)
        self.publish(1)
        dep = self.root / "temporary-dependency"
        dep.mkdir()
        (dep / "tool").write_text('#!/bin/sh\nexec /usr/bin/cc "$@"\n')
        (dep / "PKGBUILD").write_text('''pkgname=pacpin-temp-builddep
pkgver=1
pkgrel=1
arch=('any')
options=('!strip' '!debug')
package() { install -Dm755 "$startdir/tool" "$pkgdir/usr/bin/pacpin-temp-compiler"; }
''')
        run("makepkg", "--nodeps", "--noconfirm", "--nocheck", cwd=dep)
        archive = next(dep.glob("*.pkg.tar.zst"))
        shutil.copyfile(archive, self.repo / archive.name)
        run("repo-add", str(self.repo / "pacpin-ui.db.tar.gz"), str(self.repo / archive.name))
        run("sudo", "-n", "pacman", "-Sy", "--noconfirm")
        code = self.root / "code"
        code.mkdir()
        (code / "main.c").write_text('''#include <stdio.h>
#include <stdlib.h>
int main(void) { puts("TEMP_SOURCE_OK"); printf("APP_HOME=%s\\n", getenv("HOME")); return 0; }
''')
        source = self.root / "compile.tar"
        with tarfile.open(source, "w") as tar:
            tar.add(code / "main.c", arcname="main.c")
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        catalog = {"version": 1, "packages": {"temp-source": [{"tag": "v1.0.0", "version": "1.0.0", "package_version": "1.0.0-1", "assets": [{"name": "compile.tar", "url": "compile.tar", "sha256": digest}]}]}}
        catalog["packages"]["temp-prebuilt"] = [{"tag": "v1.0.0", "assets": [{"name": "source.tar", "url": "source.tar", "sha256": self.sha}]}]
        catalog["packages"]["pacpin-temp-builddep"] = [{"tag": "v1.0.0", "package_version": "1-1", "assets": [{"name": archive.name, "url": "repo/" + archive.name, "sha256": hashlib.sha256(archive.read_bytes()).hexdigest()}]}]
        (self.root / "temporary-catalog.json").write_text(json.dumps(catalog))
        self.cfg.write_text(f'''repo_order = ["pacpin-ui"]
[features]
shell_aliases = false
smart_orphans = false
integrations = false
[options]
helper = "paru"
privilege_tool = "sudo"
[sources.temporary]
kind = "local"
url = "{self.root}/temporary-catalog.json"
[sources.temporary.packages.temp-source]
asset = "compile.tar"
format = "source"
binaries = {{ temp-source = "app" }}
depends = []
[sources.temporary.packages.temp-source.build]
output = "out"
makedepends = ["pacpin-temp-builddep"]
scratch_mib = 16
scratch_inodes = 128
commands = [["mkdir", "-p", "{{output}}"], ["pacpin-temp-compiler", "{{source}}/main.c", "-o", "{{output}}/app"]]
[sources.temporary.packages.temp-prebuilt]
asset = "source.tar"
format = "archive"
binaries = {{ temp-prebuilt = "bin/app" }}
[sources.temporary.packages.pacpin-temp-builddep]
asset = "{archive.name}"
format = "native"
''')
        exporter = self.root / "exporter"
        exporter.write_text('''#!/bin/bash
set -eu
id=$1; root=$2
case "$id" in failed) exit 7;; slow) sleep 20;; esac
mkdir -p "$root/usr/bin"
if [ "$id" = escape ]; then ln -s /etc/passwd "$root/usr/bin/$id"; exit 0; fi
cat > "$root/usr/bin/$id" <<'APP'
#!/bin/bash
echo TEMP_EXPORT_OK
printf 'APP_HOME=%s\\n' "$HOME"
if [ "$1" = --write ]; then printf kept > "$HOME/pacpin-ephemeral-output"; fi
APP
chmod 755 "$root/usr/bin/$id"
''')
        run("sudo", "-n", "install", "-m755", str(exporter), "/usr/local/bin/pacpin-ephemeral-export-fixture")
        managers = self.cfg.with_name("managers.toml")
        managers.write_text('''[managers.demo]
adapter = "command"
enabled = true
[managers.demo.ephemeral]
command = ["/usr/local/bin/pacpin-ephemeral-export-fixture", "{id}", "{root}"]
executables = ["usr/bin/{id}"]
scratch_mib = 16
''')
        before = self.fingerprint()
        marker = Path.home() / "pacpin-ephemeral-output"
        if marker.exists():
            raise RuntimeError("Refusing to overwrite an existing output fixture")
        try:
            self.case("source-dry-run", ["run", "--dry-run", "--use-ram", "temporary/temp-source"])
            assert not self.workspaces(), "Dry run created a temporary workspace"
            for mode in ["run", "try"]:
                name = f"source-{mode}"
                self.case(name, [mode, "--use-storage", "--no-cache", "--noconfirm", "temporary/temp-source"], timeout=180)
                assert "TEMP_SOURCE_OK" in transcript(self.root / f"{name}.raw.txt")
                assert subprocess.run(["pacman", "-Q", "pacpin-temp-builddep"], capture_output=True).returncode != 0
                assert not Path("/usr/bin/pacpin-temp-compiler").exists()
            self.case("source-prebuilt", ["try", "--use-storage", "--noconfirm", "temporary/temp-prebuilt"], timeout=120)
            self.case("source-native", ["run", "--use-storage", "--noconfirm", "--bin", "pacpin-temp-compiler", "temporary/pacpin-temp-builddep", "--", "--version"], timeout=120)
            config_text = self.cfg.read_text()
            self.cfg.write_text(config_text.replace('commands = [["mkdir", "-p", "{output}"]', 'commands = [["false"]'))
            self.case("source-build-failure", ["run", "--use-storage", "--noconfirm", "temporary/temp-source"], success=False, expected_error="Source compilation/packaging failed", timeout=120)
            self.cfg.write_text(config_text)
            for mode in ["run", "try"]:
                name = f"export-{mode}"
                self.case(name, [mode, "--use-ram", "--no-cache", "--noconfirm", "demo/app", "--", "--write"])
                assert "TEMP_EXPORT_OK" in transcript(self.root / f"{name}.raw.txt")
                if mode == "run":
                    assert marker.read_text() == "kept", "Run did not preserve home output"
                    marker.unlink()
                else:
                    assert not marker.exists(), "Try leaked its private home output"
            self.case("export-dry-run", ["try", "--dry-run", "--use-storage", "demo/app"])
            self.case("export-failure", ["run", "--noconfirm", "demo/failed"], success=False, expected_error="Temporary export failed")
            self.case("export-escape", ["try", "--noconfirm", "demo/escape"], success=False, expected_error="Absolute symlink")
            self.case("export-cancel", ["run", "--noconfirm", "demo/slow"], success=False, cancel_manager="App", timeout=45)
            assert self.fingerprint() == before, "Temporary execution changed database, pins or persistent state"
            assert not self.workspaces(), "Temporary package/build workspace leaked"
            # Real AUR with the guest's original read-only metadata. No helper
            # or package is replaced by a fixture in this check.
            run("sudo", "-n", "cp", str(self.root / "pacman.original.conf"), "/etc/pacman.conf")
            self.cfg.write_text('[features]\nshell_aliases=false\nsmart_orphans=false\nintegrations=false\n[options]\nhelper="paru"\n')
            self.case("aur-dry-run", ["run", "--dry-run", "aur/hello"], timeout=60)
            local_before = run("pacman", "-Q")
            self.case("aur-real-build", ["run", "--use-storage", "--no-cache", "--noconfirm", "aur/hello", "--", "--version"], timeout=600)
            assert "Hello" in transcript(self.root / "aur-real-build.raw.txt")
            assert run("pacman", "-Q") == local_before, "Temporary AUR build modified guest packages"
            assert not self.workspaces(), "AUR workspace leaked"
            self.git_case()
        finally:
            marker.unlink(missing_ok=True)
            run("sudo", "-n", "rm", "-f", "/usr/local/bin/pacpin-ephemeral-export-fixture")

    def git_case(self):
        repository = self.root / "git-input"
        repository.mkdir()
        (repository / "main.c").write_text('#include <stdio.h>\nint main(void) { puts("TEMP_GIT_OK"); return 0; }\n')
        run("git", "init", "-q", str(repository))
        run("git", "-C", str(repository), "config", "user.name", "Pacpin VM fixture")
        run("git", "-C", str(repository), "config", "user.email", "fixture@example.invalid")
        run("git", "-C", str(repository), "add", "main.c")
        run("git", "-C", str(repository), "commit", "-qm", "fixture")
        run("git", "-C", str(repository), "tag", "v1.0.0")
        (self.root / "git-catalog.json").write_text(json.dumps({"version": 1, "packages": {"temp-git": [{"tag": "v1.0.0", "assets": []}]}}))
        self.cfg.write_text(f'''[features]
shell_aliases=false
smart_orphans=false
integrations=false
[sources.git-fixture]
kind="local"
url="{self.root}/git-catalog.json"
[sources.git-fixture.packages.temp-git]
asset=""
format="source"
binaries={{temp-git="app"}}
[sources.git-fixture.packages.temp-git.build]
git="{repository}"
output="out"
scratch_mib=16
scratch_inodes=128
commands=[["mkdir","-p","{{output}}"],["cc","{{source}}/main.c","-o","{{output}}/app"]]
''')
        before = self.fingerprint()
        self.case("source-git-build", ["try", "--use-storage", "--no-cache", "--noconfirm", "git-fixture/temp-git"], timeout=180)
        assert "TEMP_GIT_OK" in transcript(self.root / "source-git-build.raw.txt")
        assert self.fingerprint() == before, "Git build changed package/configuration state"
        assert not self.workspaces(), "Git temporary runtime workspace leaked"

    def fingerprint(self):
        files = [self.cfg, *self.cfg.parent.glob("*.toml"), *self.root.glob("db/local/*/desc"), *self.root.glob("state/**/*")]
        return {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in files if path.is_file()}

    def workspaces(self):
        return [str(path) for base in ["/tmp", "/var/tmp", "/dev/shm"] for pattern in ["pacpin-run-*-pid*", "pacpin-try-*-pid*"] for path in Path(base).glob(pattern)]


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--socket", default="unix:/tmp/pacpin-ui-kitty.sock")
    parser.add_argument("--section", choices=["all", "git"], default="all")
    args = parser.parse_args()
    if os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or os.geteuid() == 0 or 'ID=garuda' not in Path("/etc/os-release").read_text() or run("systemd-detect-virt").strip() != "oracle" or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay":
        raise SystemExit("Requires the user-selected disposable Garuda VirtualBox live guest as regular user")
    args.output.mkdir(exist_ok=False)
    checks = None
    try:
        checks = EphemeralChecks(args.binary, args.output, args.socket, False, False, args.section)
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Temporary backend checks failed; inspect evidence")


if __name__ == "__main__":
    main()
