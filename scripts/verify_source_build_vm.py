#!/usr/bin/env python3
"""Real makepkg C compilation, exact Git revision and pacman installation.

Run only as garuda in the disposable live VM. No host packages are modified.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import tarfile

from verify_upgrade_ui_vm import Checks, run, serialized_verifier, transcript


class BuildChecks(Checks):
    def require(self, condition, message):
        if not condition:
            self.results[-1]["failures"].append(message)

    def archive(self, source=False):
        with tarfile.open(self.artifact, "w") as archive:
            files = self.code() if source else {"bin/app": "#!/bin/sh\necho prebuilt-seed\n"}
            for name, body in files.items():
                data = body.encode()
                header = tarfile.TarInfo(name)
                header.size = len(data)
                header.mode = 0o755 if name == "bin/app" else 0o644
                archive.addfile(header, io.BytesIO(data))
        self.sha = hashlib.sha256(self.artifact.read_bytes()).hexdigest()

    def code(self):
        return {
            "main.c": '#include <stdio.h>\n#ifndef __OPTIMIZE__\n#define OPTIMIZED 0\n#else\n#define OPTIMIZED 1\n#endif\nint main(void) { printf("compiled optimized=%d\\n", OPTIMIZED); return 0; }\n',
            "Makefile": 'all:\n\t@echo PACPIN_COMPILE_ONCE\n\tmkdir -p stage/bin\n\t@printf "compile\\n" >> stage/compile-count\n\t$(CC) $(CPPFLAGS) $(CFLAGS) main.c $(LDFLAGS) -o stage/bin/app\n',
        }

    def reset(self, native=False):
        self.cfg.write_text(self.original_cfg)
        self.archive()
        self.seed()
        self.publish(2 if native else 1)
        self.archive(source=True)
        self.catalog(2)

    def configure(self, optimization=None, commands=None, git=None, output="stage", missing_dep=False):
        text = self.original_cfg.replace('format = "archive"', 'format = "source"')
        if git is not None:
            text = text.replace('asset = "source.tar"\n', '')
        text += '\n[sources.ui-local.packages.pacpin-ui-source.build]\n'
        text += 'commands = ' + json.dumps(commands or [["make"]]) + '\n'
        text += 'output = ' + json.dumps(output) + '\nscratch_mib = 8\nscratch_inodes = 64\njobs = 1\n'
        if missing_dep:
            text += 'makedepends = ["pacpin-missing-build-dependency"]\n'
        if git is not None:
            text += 'git = ' + json.dumps(str(git)) + '\n'
        if optimization:
            text += '\n[sources.ui-local.packages.pacpin-ui-source.build.optimization]\n'
            text += '\n'.join(f'{key} = {json.dumps(value)}' for key, value in optimization.items()) + '\n'
        self.cfg.write_text(text)

    def verify_compiled(self, name, optimized, cpu):
        if self.results[-1]["status"] != 0:
            return
        raw = transcript(self.root / f"{name}.raw.txt")
        self.require(run("/usr/bin/pacpin-ui-source").strip() == f"compiled optimized={optimized}", "compiled executable output differs from optimization setting")
        receipt = json.loads(Path("/opt/pacpin-ui-source/.pacpin-source.json").read_text())
        self.require(receipt["build"]["optimization"]["cpu"] == cpu, "provenance lost CPU profile")
        self.require(len(receipt["recipe_sha256"]) == 64, "build recipe digest missing")
        self.require("commands" not in receipt["build"], "literal build commands leaked into package provenance")
        # Raw PTY lines can include a footer redraw before the marker. Count
        # actual build side effects as well as the unique output marker.
        count = Path("/opt/pacpin-ui-source/compile-count").read_text().splitlines()
        self.require(count == ["compile"], f"unexpected actual build execution count: {count}")
        self.require(raw.count("PACPIN_COMPILE_ONCE") == 1, "compile output marker missing or duplicated")
        (self.root / f"{name}.build-count.json").write_text(json.dumps(count))
        frames = json.loads((self.root / f"{name}.frames.json").read_text())
        self.require(any("Sources: Building packages" in frame or "Sources: Compiling source" in frame for frame in frames), "actual compilation footer missing")

    def suite(self):
        self.original_cfg = self.cfg.read_text()
        self.base_makepkg = self.root / "makepkg-base.conf"
        self.base_makepkg.write_text(Path("/etc/makepkg.conf").read_text() + '\nCFLAGS="-O0"\nCXXFLAGS="-O0"\nRUSTFLAGS="-C opt-level=0"\nOPTIONS=(strip docs !libtool !staticlibs emptydirs zipman purge !debug !lto)\n')
        self.env["MAKEPKG_CONF"] = str(self.base_makepkg)

        self.reset()
        self.configure()
        name = "archive-build-system-default"
        self.case(name, ["-Su", "--sources-only", "--no-cache", "--noconfirm"], source=True)
        self.verify_compiled(name, 0, "system")
        self.require("-march=native" not in transcript(self.root / f"{name}.raw.txt"), "default build unexpectedly forced CPU tuning")

        self.reset(native=True)
        self.configure(optimization={"cpu": "native", "level": 2, "lto": False})
        name = "native-and-compiled-source-opt-in-resize"
        self.case(name, ["-Su", "--no-cache", "--noconfirm"], native=True, source=True, resize=True)
        self.verify_compiled(name, 1, "native")
        self.require("-march=native" in transcript(self.root / f"{name}.raw.txt"), "native compiler flag missing")

        self.reset()
        self.configure(optimization={"cpu": "native", "level": 2, "lto": True})
        name = "native-lto-explicit-opt-in"
        self.case(name, ["-Su", "--sources-only", "--noconfirm"], source=True)
        self.verify_compiled(name, 1, "native")
        self.require("-flto" in transcript(self.root / f"{name}.raw.txt"), "explicit LTO request did not reach compiler")

        self.reset()
        self.configure()
        name = "compiled-source-forced-storage"
        self.case(name, ["-Su", "--sources-only", "--use-storage", "--noconfirm"], source=True, width=40)
        self.verify_compiled(name, 0, "system")

        self.reset()
        self.configure()
        name = "compiled-source-forced-ram"
        self.case(name, ["-Su", "--sources-only", "--use-ram", "--noconfirm"], source=True, allow_ram_refusal=True)
        if self.results[-1]["status"] == 0:
            self.verify_compiled(name, 0, "system")
        else:
            self.require("Insufficient scratch capacity for Ram policy" in transcript(self.root / f"{name}.raw.txt"), "RAM refusal was not explicit")

        for name, options, error, native in [
            ("compile-failure-before-install", {"commands": [["false"]]}, "Source compilation/packaging failed", False),
            ("missing-output-before-install", {"output": "missing"}, "Source compilation/packaging failed", False),
            ("missing-build-tool-preflight", {"commands": [["pacpin-missing-build-executable"]]}, "unavailable", True),
            ("missing-build-dependency-preflight", {"missing_dep": True}, "Missing source build dependencies", True),
        ]:
            self.reset(native=native)
            self.configure(**options)
            self.case(name, ["-Su", "--no-cache", "--noconfirm"], source=True, success=False, expected_error=error)
            self.require(run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1"), "failed build changed installed source")
            self.require(run("pacman", "-Q", "pacpin-ui-native").strip().endswith("1.0.0-1"), "preflight failure allowed native changes")

        self.reset()
        git = self.root / "upstream-git"
        git.mkdir()
        for name, content in self.code().items():
            (git / name).write_text(content)
        run("git", "init", "-q", str(git))
        run("git", "-C", str(git), "add", ".")
        run("git", "-C", str(git), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "approved")
        approved = run("git", "-C", str(git), "rev-parse", "HEAD").strip()
        run("git", "-C", str(git), "tag", "v2.0.0")
        self.configure(git=git)
        def move_tag():
            (git / "main.c").write_text(self.code()["main.c"].replace("compiled", "unapproved"))
            run("git", "-C", str(git), "add", ".")
            run("git", "-C", str(git), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "later tag target")
            run("git", "-C", str(git), "tag", "-f", "v2.0.0")
        name = "git-build-keeps-approved-commit-after-tag-moves"
        self.case(name, ["-Su", "--sources-only"], source=True, confirm=True, confirm_action=move_tag)
        self.verify_compiled(name, 0, "system")
        receipt = json.loads(Path("/opt/pacpin-ui-source/.pacpin-source.json").read_text())
        self.require(receipt["git"]["commit"] == approved, "build did not retain the approved Git commit")

        self.reset()
        self.configure(commands=[["sh", "-c", "echo PACPIN_BUILD_RUNNING; sleep 60"], ["make"]])
        name = "compilation-interruption-cleans-workspace"
        self.case(name, ["-Su", "--sources-only", "--noconfirm"], source=True, success=False,
                  cancel_manager="Sources", cancel_marker="PACPIN_BUILD_RUNNING")
        self.require(run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1"), "interrupted build changed installed source")

        self.reset()
        sentinel = self.root / "sentinel"
        sentinel.write_text("keep")
        sentinel.chmod(0o600)
        self.configure(commands=[["sh", "-c", f"mkdir -p stage/bin; ln -s {sentinel} stage/bin/app"]])
        self.case("output-symlink-rejected-before-chmod", ["-Su", "--sources-only", "--noconfirm"], source=True, success=False, expected_error="Source compilation/packaging failed")
        self.require(sentinel.read_text() == "keep" and sentinel.stat().st_mode & 0o777 == 0o600, "packaging touched an unrelated file")

        self.github_case()

    def github_case(self):
        if not shutil.which("cmake"):
            raise RuntimeError("Real GitHub build check needs cmake in the disposable guest")
        self.reset()
        recipe = self.original_cfg + '''
[sources.ui-github]
kind = "github"
[sources.ui-github.packages.pacpin-ui-cmark]
project = "jgm/cmark"
format = "source"
binaries = { pacpin-ui-cmark = "bin/cmark" }
[sources.ui-github.packages.pacpin-ui-cmark.build]
git = "https://github.com/jgm/cmark.git"
output = "stage"
scratch_mib = 128
scratch_inodes = 4096
jobs = 1
commands = [
  ["cmake", "-S", "{source}", "-B", "build", "-DCMAKE_INSTALL_PREFIX={output}", "-DCMARK_TESTS=OFF", "-DCMARK_SHARED=OFF"],
  ["cmake", "--build", "build"],
  ["cmake", "--install", "build"]
]
[sources.ui-github.packages.pacpin-ui-cmark.build.optimization]
cpu = "native"
level = 2
lto = false
'''
        self.cfg.write_text(recipe)
        name = "real-github-cmark-native-build"
        self.case(name, ["-S", "ui-github/pacpin-ui-cmark", "--use-storage", "--no-cache", "--noconfirm"],
                  source=True, source_package="pacpin-ui-cmark", source_version=None, timeout=300)
        if self.results[-1]["status"] != 0:
            return
        self.require("cmark" in run("/usr/bin/pacpin-ui-cmark", "--version"), "real GitHub executable did not run")
        receipt = json.loads(Path("/opt/pacpin-ui-cmark/.pacpin-source.json").read_text())
        self.require(receipt["git"]["repository"] == "https://github.com/jgm/cmark.git" and len(receipt["git"]["commit"]) == 40, "real GitHub provenance missing")
        self.require(receipt["build"]["optimization"]["cpu"] == "native", "real GitHub CPU choice missing")


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or 'ID=garuda' not in Path("/etc/os-release").read_text() or run("systemd-detect-virt").strip() != "oracle" or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay" or os.geteuid() == 0:
        raise SystemExit("Only run as the regular user in the disposable Garuda live VM")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        raise SystemExit("Use a fresh output directory")
    checks = None
    try:
        checks = BuildChecks(args.binary, args.output, "unix:/tmp/pacpin-ui-kitty.sock", False, False, "all")
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Source build verification failed")


if __name__ == "__main__":
    main()
