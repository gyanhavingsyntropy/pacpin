#!/usr/bin/env python3
"""Real-terminal preparation checks; external adapters are controlled fixtures.

Native/Sources use real pacman/makepkg. The AUR section uses real Paru and the
upstream hello PKGBUILD; only curl's transfer rate and makepkg paths are changed.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess

from verify_upgrade_sequences_vm import SequenceChecks
from verify_upgrade_ui_vm import run, transcript, serialized_verifier, SlowRepository


class PreparationChecks(SequenceChecks):
    def close(self):
        # Real AUR packages write real guest files. Remove them through this
        # suite's still-active isolated ALPM database before restoring config;
        # a later suite must not inherit unowned hello files from an old DB.
        if self.section == "aur":
            subprocess.run(["sudo", "-n", "pacman", "-R", "--noconfirm", "hello"], capture_output=True)
        super().close()

    def seed(self, *args, **kwargs):
        preload = self.env.pop("LD_PRELOAD", None)
        try:
            super().seed(*args, **kwargs)
        finally:
            if preload:
                self.env["LD_PRELOAD"] = preload

    def fixture_preparation(self, names, fail=False):
        self.configure(names=names)
        managers = self.cfg.parent / "managers.toml"
        body = managers.read_text()
        for name in names:
            helper = self.root / f"{name}-fixture"
            record = self.root / f"{name}-actions.json"
            argv = self.root / f"{name}-argv.json"
            argv.unlink(missing_ok=True)
            code = helper.read_text()
            code = code.replace("operation = sys.argv[1]", "\n".join([
                "operation = sys.argv[1]",
                f"argv_record = Path({str(argv)!r})",
                "if operation != 'check':",
                "    entries = json.loads(argv_record.read_text()) if argv_record.exists() else []",
                "    entries.append({'argv': sys.argv[1:], 'uid': os.geteuid(), 'tmpdir': os.environ.get('TMPDIR')})",
                "    argv_record.write_text(json.dumps(entries))",
                "if operation == 'prepare':",
                "    actions.append(operation)",
                "    record.write_text(json.dumps(actions))",
                "    print('PACPIN_PROGRESS download 32/80', flush=True)",
                "    time.sleep(1.2)",
                "    print('PACPIN_PROGRESS download 80/80', flush=True)",
                "    time.sleep(.2)",
                "    sys.exit(" + ("9" if fail else "0") + ")",
            ])).replace("import json, sys, time", "import json, sys, time, os")
            helper.write_text(code)
            marker = f"[managers.{name}]\n"
            body = body.replace(marker, marker + "prepare = " + json.dumps([str(helper), "prepare", "--", "{id}", "{version}"]) + "\n")
        managers.write_text(body)

    def external_cases(self):
        self.original_cfg = self.cfg.read_text()
        entries = [
            ("external-prepare-two-managers", ("flatpak", "nix"), [], False, 80),
            ("external-prepare-cache-narrow", ("flatpak",), ["--no-cache"], False, 40),
            ("external-prepare-failure-retries", ("flatpak",), ["--no-cache"], True, 80),
            ("external-prepare-dry-run", ("flatpak",), ["--dry-run"], False, 80),
            ("external-prepare-forced-storage", ("custom",), ["--use-storage", "--no-cache"], False, 80),
        ]
        for name, names, flags, fail, width in entries:
            self.reset_case(name, native=False, source=False)
            self.fixture_preparation(names, fail)
            self.case(name, ["-Su", "--external-only", "--noconfirm", *flags], width=width)
            result = self.results[-1]
            dry = "--dry-run" in flags
            forced = "--use-storage" in flags
            frames = json.loads((self.root / f"{name}.frames.json").read_text())
            text = transcript(self.root / f"{name}.raw.txt")
            for manager in names:
                path = self.root / f"{manager}-actions.json"
                actual = json.loads(path.read_text()) if path.exists() else []
                expected = [] if dry else ([] if forced else ["prepare"]) + ["upgrade"] + (["cache"] if "--no-cache" in flags else [])
                if actual != expected:
                    result["failures"].append(f"{manager} order/effects {actual} != {expected}")
                argv = self.root / f"{manager}-argv.json"
                calls = json.loads(argv.read_text()) if argv.exists() else []
                if any(entry["uid"] == 0 for entry in calls):
                    result["failures"].append("external preparation ran elevated")
                prepared = [entry["argv"] for entry in calls if entry["argv"][0] == "prepare"]
                if not dry and not forced and prepared != [["prepare", "--", "org.pacpin.Test", "2.0"]]:
                    result["failures"].append("preparation lost approved target/version")
                if not dry and not forced and not any(f"{manager.title()}: Downloading (operation) (40%)" in frame for frame in frames):
                    result["failures"].append(f"{manager} preparation percent missing")
            if fail and "preparation failed:" not in text:
                result["failures"].append("preparation failure/retry not explained")
            result["external_adapters"] = "controlled literal-command fixtures"
            self.checkpoint()

        # No explicit preparation is still an eligible upgrade, not a skip.
        self.reset_case("external-unsupported-preparation", native=False, source=False)
        self.configure(names=("nix", "pipx"))
        self.case("external-unsupported-preparation", ["-Su", "--external-only", "--noconfirm"])
        for manager in ("nix", "pipx"):
            if json.loads((self.root / f"{manager}-actions.json").read_text()) != ["upgrade"]:
                self.results[-1]["failures"].append("optional capability gap excluded normal upgrade")
        self.checkpoint()

        self.reset_case("external-prepare-no-work", native=False, source=False)
        self.configure(names=("flatpak",), empty=True)
        managers = self.cfg.parent / "managers.toml"
        managers.write_text(managers.read_text().replace("[managers.flatpak]\n", "[managers.flatpak]\nprepare = " + json.dumps([str(self.root / "flatpak-fixture"), "prepare", "--", "{id}"]) + "\n"))
        self.case("external-prepare-no-work", ["-Su", "--external-only", "--noconfirm"])
        if (self.root / "flatpak-actions.json").exists():
            self.results[-1]["failures"].append("no-work manager prepared downloads")
        self.checkpoint()

        self.reset_case("external-skipped-no-preparation", native=True, source=False)
        self.fixture_preparation(("flatpak",))
        managers.write_text("\n".join(line for line in managers.read_text().splitlines() if not line.startswith("cache_cleanup =")) + "\n")
        self.case("external-skipped-no-preparation", ["-Su", "--keep-going", "--no-cache", "--noconfirm"], native=True,
                  success=False, expected_error="Flatpak skipped (--keep-going")
        if (self.root / "flatpak-actions.json").exists():
            self.results[-1]["failures"].append("skipped manager prepared/upgraded/cleaned")
        if not run("pacman", "-Q", "pacpin-ui-native").strip().endswith("2.0.0-1"):
            self.results[-1]["failures"].append("eligible native upgrade did not complete")
        self.checkpoint()

    def source_cases(self):
        self.original_cfg = self.cfg.read_text()
        for name, native, flags, resize in [
            ("source-only-preparation", False, ["--sources-only"], False),
            ("native-source-preparation", True, [], False),
            ("native-source-preparation-resize", True, ["--no-cache"], True),
            ("source-preparation-interrupt", False, ["--sources-only"], False),
        ]:
            self.reset_case(name, native=native, source=True)
            SlowRepository.download_delay = .1
            cancel = name.endswith("interrupt")
            self.case(name, ["-Su", "--noconfirm", *flags], native=native, source=True,
                      resize=resize, cancel=cancel, success=not cancel, timeout=90)
            result = self.results[-1]
            frames = json.loads((self.root / f"{name}.frames.json").read_text())
            if native:
                for frame in frames:
                    if "Pacman: Downloading packages" in frame and re.search(r"Sources: Downloading.*MiB", frame):
                        result["failures"].append("Sources started before native downloads finished")
                        break
            if (self.root / "db/db.lck").exists():
                result["failures"].append("ALPM lock leaked")
            self.checkpoint()

        for name, command in [("native-only-upgrade", ["-Su"]), ("ordinary-native-install", ["-S", "pacpin-ui/pacpin-ui-native"])]:
            self.reset_case(name, native=True, source=False)
            self.case(name, [*command, "--noconfirm"], native=True)
            self.checkpoint()

        name = "native-download-failure-no-source-preparation"
        self.reset_case(name, native=True, source=True)
        for archive in self.repo.glob("*.pkg.tar.zst"):
            archive.unlink()
        self.case(name, ["-Su", "--noconfirm", "--no-cache"], native=True, source=True, success=False,
                  expected_error="failed retrieving file", cache=False)
        frames = json.loads((self.root / f"{name}.frames.json").read_text())
        if any(re.search(r"(?:^|\n)Sources: Downloading", frame) for frame in frames):
            self.results[-1]["failures"].append("queued Sources download started during failed native download")
        if not run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1"):
            self.results[-1]["failures"].append("Sources installed despite fatal native failure")
        self.checkpoint()

        # Controlled helper emits real makepkg's charset/reset shape. Sources
        # still package/install through actual makepkg/pacman after the guarded
        # failure permit; the original AUR failure remains nonzero and visible.
        name = "colored-aur-failure-source-continuation"
        self.reset_case(name, native=False, source=True)
        helpers = self.root / "failure-helper"
        helpers.mkdir()
        helper = helpers / "paru"
        helper.write_text("#!/bin/sh\nprintf '\\033[1m==>\\033(B\\033[m Starting build()...\\n'\nsleep .2\nprintf '\\033[1;31m==> ERROR:\\033(B\\033[m A failure occurred in build().\\n'\nexit 1\n")
        helper.chmod(0o755)
        self.cfg.write_text(self.cfg.read_text().replace('helper = "paru"', f'helper = "{helper}"').replace('[pins]', '[pins]\nhello = "aur"'))
        package = self.root / "db/local/hello-0.0.0-1"
        package.mkdir()
        (package / "desc").write_text("%NAME%\nhello\n\n%VERSION%\n0.0.0-1\n\n%REASON%\n0\n\n%SIZE%\n1\n\n%ARCH%\nx86_64\n\n")
        (package / "files").write_text("%FILES%\n\n")
        self.case(name, ["-Su", "--keep-going", "--noconfirm"], source=True, source_completed=True,
                  aur=True, success=False, expected_error="AUR build failed without observed native changes")
        self.results[-1]["aur_runner"] = "controlled colored build-failure helper; real guarded Sources installation"
        if not run("pacman", "-Q", "pacpin-ui-source").strip().endswith("2.0.0-1"):
            self.results[-1]["failures"].append("recognized colored AUR failure did not allow guarded Sources continuation")
        self.checkpoint()

    def aur_cases(self):
        self.original_cfg = self.cfg.read_text()
        build = self.root / "aur-old"
        build.mkdir()
        (build / "PKGBUILD").write_text("pkgname=hello\npkgver=0.0.0\npkgrel=1\narch=('x86_64')\npackage() { install -Dm644 /etc/os-release \"$pkgdir/opt/pacpin-ui-aur/placeholder\"; }\n")
        run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=build)
        old = next(build.glob("*.pkg.tar.zst"))
        paru = self.root / "config/paru"
        paru.mkdir()
        (paru / "paru.conf").write_text("[options]\nSkipReview\nRebuild\n")
        base = Path("/etc/makepkg.conf").read_text()
        for name, native, source, command in [
            ("aur-only-transfer", False, False, ["-Su", "--aur-only"]),
            ("aur-source-transfer", False, True, ["-Su"]),
            ("native-aur-source-transfer", True, True, ["-Su"]),
            ("ordinary-aur-install-transfer", False, False, ["-S", "aur/hello"]),
        ]:
            self.reset_case(name, native=native, source=source)
            self.cfg.write_text(self.cfg.read_text().replace('[pins]', '[pins]\nhello = "aur"'))
            run("sudo", "-n", "pacman", "-U", "--noconfirm", str(old))
            scratch = self.root / f"makepkg-{name}"
            scratch.mkdir()
            config = scratch / "makepkg.conf"
            # Real makepkg curl transfer; separate source/package caches force
            # the download every time without replacing Paru or its output.
            config.write_text(base + "\nOPTIONS=(strip docs !libtool !staticlibs emptydirs zipman purge !debug !lto)\n"
                              + f"SRCDEST={str(scratch)!r}\nPKGDEST={str(scratch)!r}\n"
                              + r'for i in "${!DLAGENTS[@]}"; do DLAGENTS[$i]="${DLAGENTS[$i]/\/usr\/bin\/curl/\/usr\/bin\/curl --limit-rate 96k}"; done' + "\n")
            self.env["MAKEPKG_CONF"] = str(config)
            self.case(name, [*command, "--noconfirm"], native=native, source=source, aur=True, timeout=300)
            result = self.results[-1]
            frames = json.loads((self.root / f"{name}.frames.json").read_text())
            text = transcript(self.root / f"{name}.raw.txt")
            if not any(re.search(r"AUR: Downloading \(file\) \((?:[1-9]|[1-9]\d)%\)", frame) for frame in frames):
                result["failures"].append("no intermediate real AUR file percentage rendered")
            if source and not native:
                before_build = text.split("==> Starting build()", 1)[0]
                if "Sources: Downloading" in before_build or "Sources: source.tar" in before_build:
                    result["failures"].append("Sources started during initial AUR fetch")
                if not any("AUR: Building packages" in frame and "Sources: Downloading" in frame for frame in frames):
                    result["failures"].append("Sources did not overlap actual AUR build")
            if native and not any("Pacman: Downloading" in frame and "AUR: Pending" in frame for frame in frames):
                result["failures"].append("AUR pending footer missing during native download")
            if source and not any("AUR: ✔ Upgrade complete" in frame and re.search(r"(?:^|\n)Sources:", frame) for frame in frames):
                result["failures"].append("completed AUR row missing during Sources")
            result["aur_runner"] = "real Paru/makepkg/upstream hello; curl --limit-rate 96k"
            self.checkpoint()

    def suite(self):
        if self.section == "aur":
            self.aur_cases()
        elif self.section == "external":
            self.external_cases()
        else:
            self.external_cases()
            self.source_cases()


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--section", choices=["core", "aur", "external"], default="core")
    args = parser.parse_args()
    if (os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or os.geteuid() == 0
            or "ID=garuda" not in Path("/etc/os-release").read_text()
            or run("systemd-detect-virt").strip() != "oracle"
            or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay"):
        raise SystemExit("Requires regular user in disposable Garuda live VM")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        raise SystemExit("Use a fresh output directory")
    checks = None
    try:
        checks = PreparationChecks(args.binary, args.output, "unix:/tmp/pacpin-ui-kitty.sock", False, False, args.section)
        checks.job_control = True
        checks.sudo_dir = None
        checks.suite()
    finally:
        if checks:
            checks.checkpoint()
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(item["failures"] for item in checks.results):
        raise SystemExit("Preparation checks failed")


if __name__ == "__main__":
    main()
