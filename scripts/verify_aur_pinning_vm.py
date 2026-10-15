#!/usr/bin/env python3
"""Guest-only pin persistence checks with real makepkg/pacman and a helper fixture.

The helper deliberately installs an unrequested dependency. It is controlled
test code, not evidence of a real Paru build or upstream VCS commit discovery.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import tomllib

from verify_upgrade_ui_vm import Checks, run, serialized_verifier


class PinChecks(Checks):
    def real_paru_case(self):
        self.publish(1)
        self.cfg.write_text('repo_order = ["pacpin-ui"]\n[features]\nshell_aliases = false\n'
                            'smart_orphans = false\nintegrations = false\n[options]\n'
                            'helper = "paru"\nprivilege_tool = "sudo"\n[pins]\nkept-rule = "extra"\n')
        paru_config = self.root / "config/paru"
        paru_config.mkdir(exist_ok=True)
        (paru_config / "paru.conf").write_text("[options]\nSkipReview\n")
        makepkg_config = self.root / "real-paru-makepkg.conf"
        makepkg_config.write_text(Path("/etc/makepkg.conf").read_text()
                                 + "\nOPTIONS=(strip docs !libtool !staticlibs emptydirs zipman purge !debug !lto)\n")
        self.env["MAKEPKG_CONF"] = str(makepkg_config)
        self.case("real-paru-bare-install-no-pin", ["-S", "hello", "--noconfirm"], aur=True, timeout=300)
        result = self.results[-1]
        pins = tomllib.loads(self.cfg.read_text()).get("pins", {})
        if pins != {"kept-rule": "extra"}:
            result["failures"].append(f"real Paru created unexpected pins: {pins}")
        if (self.root / "db/db.lck").exists():
            result["failures"].append("real Paru left an ALPM lock")
        result.update(pins=pins, helper="real Paru, upstream AUR hello recipe and GNU source")

    def package(self, name):
        build = self.root / name
        build.mkdir()
        (build / "PKGBUILD").write_text(
            f"pkgname={name}\npkgver=2.0\npkgrel=1\narch=('x86_64')\n"
            "options=('!strip' '!debug')\npackage() {\n"
            f" install -Dm644 /etc/os-release \"$pkgdir/opt/{name}/fixture\"\n}}\n")
        run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=build)
        return next(build.glob("*.pkg.tar.zst"))

    def suite(self):
        self.publish(1)
        app = self.package("hello")
        dependency = self.package("pacpin-pin-dependency-git")
        helper = self.root / "tools/paru"
        helper.parent.mkdir()
        fail = self.root / "helper-failure"
        helper.write_text(
            "#!/bin/sh\nset -eu\n"
            "if [ \"${1:-}\" = --version ]; then echo 'Paru controlled pin fixture'; exit 0; fi\n"
            "sleep 1\n"
            f"if [ -e {shlex.quote(str(fail))} ]; then echo PACPIN_PIN_HELPER_FAILED >&2; exit 23; fi\n"
            "echo PACPIN_PIN_DEPENDENCY_DISCOVERY_FIXTURE\n"
            "sleep 1\n"
            f"sudo -n pacman -U --asdeps --noconfirm -- {shlex.quote(str(dependency))}\n"
            f"sudo -n pacman -U --noconfirm -- {shlex.quote(str(app))}\n")
        helper.chmod(0o755)
        template = (
            'repo_order = ["pacpin-ui"]\n[features]\n'
            'shell_aliases = false\nsmart_orphans = false\nintegrations = false\n'
            f'[options]\nhelper = "{helper}"\nprivilege_tool = "sudo"\n'
            '[pins]\nkept-rule = "extra"\n"lib*" = "extra"\n')
        baseline = {"kept-rule": "extra", "lib*": "extra"}
        cases = [
            ("bare-aur-install-does-not-pin", ["-S", "hello", "--noconfirm"], True, {}),
            ("qualified-aur-pins-target-only", ["-S", "aur/hello", "--noconfirm"], True, {"hello": "aur"}),
            ("qualified-native-still-pins", ["-S", "pacpin-ui/pacpin-ui-native", "--noconfirm"], True, {"pacpin-ui-native": "pacpin-ui"}),
            ("bare-native-does-not-pin", ["-S", "pacpin-ui-native", "--noconfirm"], True, {}),
            ("failed-aur-does-not-pin", ["-S", "aur/hello", "--noconfirm"], False, {}),
            ("dry-run-does-not-pin", ["-S", "aur/hello", "--dry-run"], True, {}),
        ]
        for name, args, success, extra in cases:
            self.cfg.write_text(template)
            for package in ["hello", "pacpin-pin-dependency-git", "pacpin-ui-native"]:
                subprocess.run(["sudo", "-n", "pacman", "-R", "--noconfirm", package], capture_output=True)
            if not success:
                fail.touch()
            else:
                fail.unlink(missing_ok=True)
            aur = "aur" in name and "dry-run" not in name
            native = "native" in name
            if native:
                run("sudo", "-n", "pacman", "-U", "--noconfirm", str(self.native[1]))
                self.publish(2)
            self.case(name, args, success=success, aur=aur, native=native, timeout=90)
            result = self.results[-1]
            pins = tomllib.loads(self.cfg.read_text()).get("pins", {})
            if pins != baseline | extra:
                result["failures"].append(f"pin persistence mismatch: {pins}")
            if success and aur:
                deps = run("pacman", "-Qqd").splitlines()
                if "pacpin-pin-dependency-git" not in deps:
                    result["failures"].append("implicit dependency was not installed as a dependency")
            if (self.root / "db/db.lck").exists():
                result["failures"].append("ALPM lock left behind")
            result["pins"] = pins
            result["helper"] = "controlled fixture; real makepkg and pacman"


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--real-paru-only", action="store_true")
    args = parser.parse_args()
    if (os.environ.get("PACPIN_DISPOSABLE_VM") != "1"
            or "ID=garuda" not in Path("/etc/os-release").read_text()
            or run("systemd-detect-virt").strip() != "oracle"
            or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay"
            or os.geteuid() == 0):
        raise SystemExit("Requires regular user in disposable Garuda live VM")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        raise SystemExit("Use a fresh evidence directory")
    checks = None
    try:
        checks = PinChecks(args.binary, args.output, "unix:/tmp/pacpin-ui-kitty.sock", False, False, "all")
        if args.real_paru_only:
            checks.real_paru_case()
        else:
            checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Pin persistence checks failed")


if __name__ == "__main__":
    main()
