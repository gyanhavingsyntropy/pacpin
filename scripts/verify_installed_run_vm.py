#!/usr/bin/env python3
"""Real installed/repository try/run regressions, disposable Garuda only."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import uuid

from verify_upgrade_ui_vm import Checks, run, serialized_verifier, transcript


class InstalledChecks(Checks):
    def suite(self):
        self.seed(source=False, native=False)
        self.publish(1)
        name = "pacpin-ui-exec"
        archives = {}
        for version in [1, 2]:
            build = self.root / f"exec-build-{version}"
            build.mkdir()
            app = build / "app"
            app.write_text(f'''#!/bin/bash
printf 'PACPIN_EXEC_VERSION={version}\\n'
if [ -f "$HOME/.pacpin-exec-host-sentinel" ]; then echo HOST_HOME_VISIBLE; else echo HOME_PRIVATE; fi
printf 'PACPIN_APP_PATH=%s\\n' "$0"
printf 'PACPIN_APP_ARGS=%s\\n' "$*"
stat -f -c 'PACPIN_APP_FS=%T' "$0"
if [ "$1" = --write-files ]; then
  for directory in "$HOME" /tmp "$PWD"; do
    if mkdir "$directory/$2"; then printf 'persistent-or-private\\n' > "$directory/$2/output"; echo "WRITE_OK=$directory"; else echo "WRITE_REFUSED=$directory"; fi
  done
fi
''')
            (build / "PKGBUILD").write_text(f'''pkgname={name}
pkgver={version}
pkgrel=1
arch=('x86_64')
options=('!strip' '!debug')
{ "depends=('pacpin-ui-run-dep')" if version == 2 else "" }
package() {{
  install -Dm755 "$startdir/app" "$pkgdir/usr/bin/{name}"
  install -Dm755 "$startdir/app" "$pkgdir/usr/lib/pacpin-exec-fixture/bin/java"
}}
''')
            run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=build)
            archives[version] = next(build.glob("*.pkg.tar.zst"))
        dependency = self.root / "exec-dependency-build"
        dependency.mkdir()
        (dependency / "data").write_bytes(os.urandom(1024))
        (dependency / "tool").write_text('#!/bin/sh\necho DEPENDENCY_TOOL_MUST_NOT_RUN\n')
        (dependency / "PKGBUILD").write_text('''pkgname=pacpin-ui-run-dep
pkgver=1
pkgrel=1
arch=('any')
options=('!strip' '!debug')
package() {
  install -Dm644 "$startdir/data" "$pkgdir/usr/share/pacpin-run-fixture/data"
  install -Dm755 "$startdir/tool" "$pkgdir/usr/bin/pacpin-ui-run-dependency-tool"
}
''')
        run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=dependency)
        dependency_archive = next(dependency.glob("*.pkg.tar.zst"))
        run("sudo", "-n", "pacman", "-U", "--noconfirm", str(archives[1]))
        marker = Path.home() / ".pacpin-exec-host-sentinel"
        if marker.exists():
            raise RuntimeError("Refusing to overwrite an existing guest sentinel")
        marker.write_text("disposable guest credential isolation fixture")
        project = Path("/home") / f"pacpin-exec-project-{uuid.uuid4().hex}"
        run("sudo", "-n", "install", "-d", "-o", str(os.getuid()), "-g", str(os.getgid()), "-m", "700", str(project))
        try:
            for mode in ["run", "try"]:
                for flag in [[], ["--installed"]]:
                    case = f"{mode}-installed-{'explicit' if flag else 'fallback'}"
                    self.case(case, [mode, *flag, name, "--", "--version"])
                    text = transcript(self.root / f"{case}.raw.txt")
                    assert "already installed (1-1)" in text and "PACPIN_EXEC_VERSION=1" in text
                    assert ("HOST_HOME_VISIBLE" if mode == "run" else "HOME_PRIVATE") in text
                    assert "Resolving download URLs" not in text
            self.case("installed-bin-must-belong", ["run", "--installed", "--bin", "bash", name], success=False, expected_error="not found in package")
            self.case("installed-toolkit-bin", ["run", "--installed", "--bin", "java", name])
            assert "PACPIN_EXEC_VERSION=1" in transcript(self.root / "installed-toolkit-bin.raw.txt")
            for mode, flags in [("run", []), ("try", []), ("try", ["--rw"])]:
                case = f"{mode}-output-persistence-{'rw' if flags else 'default'}"
                token = f"pacpin-output-{uuid.uuid4().hex}"
                paths = [Path.home() / token, Path("/tmp") / token, project / token]
                try:
                    self.case(case, [mode, "--installed", *flags, name, "--", "--write-files", token], cwd=project)
                    assert paths[0].exists() == (mode == "run")
                    assert paths[1].exists() == (mode == "run")
                    assert paths[2].exists() == (mode == "run" or bool(flags))
                    text = transcript(self.root / f"{case}.raw.txt")
                    assert f"WRITE_OK={Path.home()}" in text and "WRITE_OK=/tmp" in text
                    assert (f"WRITE_REFUSED={project}" if mode == "try" and not flags else f"WRITE_OK={project}") in text
                finally:
                    for path in paths:
                        if path.exists():
                            shutil.rmtree(path)
            self.case("installed-prefix-conflict", ["run", "--installed", f"pacpin-ui/{name}"], success=False, expected_error="unqualified")
            self.case("explicit-repo-never-falls-back", ["run", f"pacpin-ui/{name}"], success=False, expected_error="not found in repository")
            self.case("explicit-installed-missing", ["run", "--installed", "pacpin-ui-not-installed"], success=False, expected_error="not installed")
            shutil.copyfile(archives[2], self.repo / archives[2].name)
            shutil.copyfile(dependency_archive, self.repo / dependency_archive.name)
            run("repo-add", str(self.repo / "pacpin-ui.db.tar.gz"), str(self.repo / archives[2].name), str(self.repo / dependency_archive.name))
            run("sudo", "-n", "pacman", "-Sy", "--noconfirm")
            self.case("repository-toolkit-bin", ["run", "--bin", "java", f"pacpin-ui/{name}"])
            assert "PACPIN_EXEC_VERSION=2" in transcript(self.root / "repository-toolkit-bin.raw.txt")
            for mode in ["run", "try"]:
                case = f"{mode}-dependency-bin-refused"
                self.case(case, [mode, "--bin", "pacpin-ui-run-dependency-tool", f"pacpin-ui/{name}"], success=False, expected_error="not found in package")
                assert "DEPENDENCY_TOOL_MUST_NOT_RUN" not in transcript(self.root / f"{case}.raw.txt")
            for mode in ["try", "run"]:
                case = f"{mode}-newer-repository-keeps-installed"
                self.case(case, [mode, f"pacpin-ui/{name}", "--", "--version"])
                text = transcript(self.root / f"{case}.raw.txt")
                assert "already installed (1-1)" in text and "PACPIN_EXEC_VERSION=2" in text
                assert "Testing the repository package" in text
                assert run("pacman", "-Q", name).strip() == f"{name} 1-1"
            self.case("installed-overrides-available-newer-repository", ["run", "--installed", name])
            assert "PACPIN_EXEC_VERSION=1" in transcript(self.root / "installed-overrides-available-newer-repository.raw.txt")
            assert run("pacman", "-Q", name).strip() == f"{name} 1-1"
            for mode in ["run", "try"]:
                case = f"{mode}-storage-no-cache"
                slow_tools = self.root / "slow-download-tools"
                slow_tools.mkdir(exist_ok=True)
                curl = slow_tools / "curl"
                curl.write_text('#!/bin/sh\nexec /usr/bin/curl --limit-rate 64 "$@"\n')
                curl.chmod(0o755)
                original_path = self.env["PATH"]
                self.env["PATH"] = f"{slow_tools}:{original_path}"
                self.case(case, [mode, "--use-storage", "--no-cache", f"pacpin-ui/{name}", "--", "--use-ram", "--pkexec"], width=44)
                self.env["PATH"] = original_path
                text = transcript(self.root / f"{case}.raw.txt")
                assert "PACPIN_EXEC_VERSION=2" in text and "(Storage)" in text
                assert "PACPIN_APP_ARGS=--use-ram --pkexec" in text
                assert "checksum verification complete" in text and "Extracting package" in text
                workspace = next(line.split("workspace: ", 1)[1].split(" (Storage)", 1)[0] for line in text.splitlines() if "Temporary app workspace:" in line)
                assert workspace.startswith("/var/tmp/") and not Path(workspace).exists()
                frames = json.loads((self.root / f"{case}.frames.json").read_text())
                assert any("App:" in frame and "Download" in frame and "%" in frame and "KiB" in frame for frame in frames)
                assert "Found 2 package(s)" in text
                self.case(f"{mode}-ram-reserve-refusal", [mode, "--use-ram", f"pacpin-ui/{name}"], success=False, expected_error="2.5 GiB OS reserve")
                self.case(f"{mode}-preview-no-app-execution", [mode, "--dry-run", "--use-storage", "--no-cache", f"pacpin-ui/{name}"])
                text = transcript(self.root / f"{mode}-preview-no-app-execution.raw.txt")
                assert "Dry-run:" in text and "PACPIN_EXEC_VERSION" not in text and "Fetching '" not in text
                self.case(f"{mode}-installed-scratch-refused", [mode, "--installed", "--use-storage", name], success=False, expected_error="repository archive to relocate")
            for provider in ["flatpak/org.gnome.Calculator", "nix/hello", "pipx/pyfiglet"]:
                case = "scratch-unsupported-" + provider.split('/')[0]
                self.case(case, ["run", "--use-ram", provider], success=False, expected_error="unsupported")
            for provider in ["flatpak/org.gnome.Calculator", "nix/hello"]:
                self.case("cache-unsupported-" + provider.split('/')[0], ["run", "--no-cache", provider], success=False, expected_error="unsupported")
            self.case("pipx-cache-preview", ["run", "--dry-run", "--no-cache", "pipx/pyfiglet"])
            assert "environment reuse disabled" in transcript(self.root / "pipx-cache-preview.raw.txt")
            self.case("scratch-conflict", ["run", "--use-ram", "--use-storage", name], success=False, expected_error="mutually exclusive")
            self.case("privilege-conflict", ["run", "--sudo", "--pkexec", name], success=False, expected_error="mutually exclusive")
            assert run("pacman", "-Q", name).strip() == f"{name} 1-1"
            result = subprocess.run(["pacman", "-Q", "pacpin-ui-run-dep"], capture_output=True)
            assert result.returncode != 0, "Ephemeral dependency was installed permanently"
        finally:
            marker.unlink(missing_ok=True)
            run("sudo", "-n", "pacman", "-R", "--noconfirm", name)
            run("sudo", "-n", "rmdir", str(project))


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--socket", default="unix:/tmp/pacpin-ui-kitty.sock")
    args = parser.parse_args()
    if os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or os.geteuid() == 0 or 'ID=garuda' not in Path("/etc/os-release").read_text() or run("systemd-detect-virt").strip() != "oracle" or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay":
        raise SystemExit("Requires the disposable Garuda VirtualBox live guest as its regular user")
    args.output.mkdir(exist_ok=False)
    checks = None
    try:
        checks = InstalledChecks(args.binary, args.output, args.socket, False, False, "all")
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Installed try/run scenarios failed; inspect evidence")


if __name__ == "__main__":
    main()
