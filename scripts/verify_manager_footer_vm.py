#!/usr/bin/env python3
"""Real Kitty UI checks; external and AUR helpers are controlled fixtures.

Native and Sources use real pacman/makepkg in the disposable live VM.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

from verify_capabilities_vm import CapabilityChecks
from verify_upgrade_ui_vm import run, serialized_verifier, transcript


class FooterChecks(CapabilityChecks):
    def configure_footer(self, empty=False, failed_check=False):
        self.configure(names=("flatpak", "nix"), empty=empty)
        for name in ["flatpak", "nix"]:
            helper = self.root / f"{name}-fixture"
            text = helper.read_text().replace("if operation == 'check':", "if operation == 'check':\n    time.sleep(3)")
            if failed_check and name == "nix":
                text = text.replace("    time.sleep(3)", "    time.sleep(3)\n    sys.exit('controlled Nix metadata failure')")
            helper.write_text(text.replace("time.sleep(1)", "time.sleep(2)"))

    def require(self, condition, message):
        if not condition:
            self.results[-1]["failures"].append(message)

    def frames(self, name):
        return json.loads((self.root / f"{name}.frames.json").read_text())

    def discovery_cases(self):
        self.configure_footer()
        self.seed()
        self.publish(2)
        self.catalog(1)
        name = "read-only-named-discovery"
        self.case(name, ["check"])
        self.require(any("Flatpak: Checking updates" in f and "Nix: Checking updates" in f for f in self.frames(name)), "read-only check lacks concurrent manager rows")
        self.require(run("pacman", "-Q", "pacpin-ui-native").strip().endswith("1.0.0-1"), "read-only check changed installed packages")
        self.require(not (self.root / "flatpak-actions.json").exists(), "read-only check executed an upgrade action")
        name = "quiet-check-without-footer"
        self.case(name, ["check", "--quiet"])
        self.require("pacpin-ui-native" in transcript(self.root / f"{name}.raw.txt"), "quiet check lost update names")
        self.require("\x1b" not in transcript(self.root / f"{name}.raw.txt"), "quiet check emitted terminal controls")
        managers = self.cfg.parent / "managers.toml"
        text = managers.read_text()
        for manager in ["flatpak", "nix"]:
            helper = self.root / f"{manager}-fixture"
            check = "check = " + json.dumps([str(helper), "check"])
            text = text.replace(check, "refresh = " + json.dumps([str(helper), "refresh"]) + "\n" + check)
        managers.write_text(text)
        name = "external-refresh-separate-manager-rows"
        self.case(name, ["-Syu", "--external-only", "--noconfirm"])
        self.require(any("Flatpak: Refreshing metadata" in f and "Nix: Refreshing metadata" in f for f in self.frames(name)), "concurrent refresh rows missing")
        for manager in ["flatpak", "nix"]:
            self.require(json.loads((self.root / f"{manager}-actions.json").read_text()) == ["refresh", "upgrade"], "refresh/upgrade invocation mismatch")

    def suite(self):
        for name, scope, width in [
            ("native-source-flatpak-nix-footer", [], 80),
            ("external-only-flatpak-nix-footer", ["--external-only"], 40),
        ]:
            self.configure_footer()
            self.seed()
            self.publish(2)
            self.catalog(2 if not scope else 1)
            self.case(name, ["-Su", *scope, "--no-cache", "--noconfirm"], native=not scope, source=not scope, width=width, orphans=False)
            frames = self.frames(name)
            self.require(any("Flatpak: Checking updates" in f and "Nix: Checking updates" in f for f in frames), "concurrent named discovery rows missing")
            if not scope:
                self.require(any("Pacman: Downloading" in f and "Flatpak: Pending" in f and "Nix: Pending" in f for f in frames), "external pending stages missing during Pacman download")
            self.require(any("Flatpak: ✔ Upgrade complete" in f and re.search(r"(?:^|\n)Nix: (?!Pending|✔)", f) for f in frames), "completed Flatpak row missing during Nix work")
            for manager in ["flatpak", "nix"]:
                self.require(json.loads((self.root / f"{manager}-actions.json").read_text()) == ["upgrade", "cache"], "external upgrade/cache invocation mismatch")
            if not scope:
                self.require("Native cache cleaned successfully" in transcript(self.root / f"{name}.raw.txt"), "native cache cleanup not confirmed")
            self.require(not any("External: Download" in f for f in frames), "generic external download replaces named stages")
            self.require(not any(re.search(r"(?:^|\n)AUR:", f) for f in frames), "unplanned AUR stage appeared")

        self.configure_footer(empty=True)
        self.seed()
        self.publish(2)
        self.catalog(1)
        name = "checked-but-no-external-updates"
        self.case(name, ["-Su", "--noconfirm"], native=True)
        frames = self.frames(name)
        self.require(any("Flatpak: Checking updates" in f and "Nix: Checking updates" in f for f in frames), "no-work discovery missing")
        self.require(not any(re.search(r"(?:Flatpak|Nix): (?:Pending|✔)", f) for f in frames), "check completion was treated as an upgrade")

        self.configure_footer(failed_check=True)
        self.seed()
        self.publish(2)
        self.catalog(1)
        name = "named-external-check-failure"
        self.case(name, ["-Su", "--keep-going", "--noconfirm"], success=False, expected_error="Nix")
        self.require(not (self.root / "flatpak-actions.json").exists(), "external upgrade started despite invalid metadata")
        self.require(run("pacman", "-Q", "pacpin-ui-native").strip().endswith("1.0.0-1"), "native transaction started after metadata failure")

        self.configure_footer(empty=True)
        self.seed()
        self.publish(2)
        self.catalog(1)
        original_cfg = self.cfg.read_text()
        package = self.root / "db/local/hello-0.0.0-1"
        package.mkdir()
        (package / "desc").write_text("%NAME%\nhello\n\n%VERSION%\n0.0.0-1\n\n%REASON%\n0\n\n%SIZE%\n1\n\n%ARCH%\nx86_64\n\n")
        (package / "files").write_text("%FILES%\n\n")
        helper_dir = self.root / "controlled-aur"
        helper_dir.mkdir()
        helper = helper_dir / "paru"
        helper.write_text("#!/bin/sh\necho '==> Making package: controlled AUR fixture'\nsleep 2\necho 'Controlled AUR helper failure'\nexit 7\n")
        helper.chmod(0o755)
        self.cfg.write_text(original_cfg.replace('helper = "paru"', f'helper = "{helper}"').replace('[pins]', '[pins]\nhello = "aur"'))
        try:
            name = "aur-pending-during-native-download"
            self.case(name, ["-Su", "--noconfirm"], native=True, aur=True, success=False, expected_error="Controlled AUR helper failure", width=40)
            frames = self.frames(name)
            self.require(any("Pacman: Downloading" in f and "AUR: Pending" in f for f in frames), "AUR pending stage missing during native download")
            self.require(any("Pacman: ✔ Upgrade complete" in f and "AUR: Building" in f for f in frames), "AUR did not transition from pending to actual helper work")
            self.require(not any("AUR: ✔" in f for f in frames), "failed AUR marked complete")
        finally:
            self.cfg.write_text(original_cfg)
            shutil.rmtree(package)

        self.configure_footer()
        self.seed()
        self.publish(2)
        self.catalog(2)
        name = "native-source-external-resize"
        self.case(name, ["-Su", "--noconfirm"], native=True, source=True, resize=True)
        frames = self.frames(name)
        self.require(any("Flatpak: Pending" in f and "Nix: Pending" in f for f in frames), "pending managers disappeared after resize")
        self.configure_footer()
        self.seed()
        self.publish(2)
        self.catalog(1)
        output = self.root / "redirected.txt"
        with output.open("w") as stream:
            result = subprocess.run([str(self.binary), "-Su", "--external-only", "--noconfirm"], env=self.env, stdout=stream, stderr=subprocess.STDOUT)
        text = output.read_text()
        self.results.append({"case": "redirected-no-terminal-controls", "status": result.returncode, "failures": []})
        self.require(result.returncode == 0 and "\x1b" not in text, "redirected output contains terminal controls or failed")
        for label in ["Flatpak", "Nix"]:
            self.require(f"{label} upgrade completed successfully" in text, "redirected output lost named completion")


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--section", choices=["all", "discovery"], default="all")
    args = parser.parse_args()
    if os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or 'ID=garuda' not in Path("/etc/os-release").read_text() or run("systemd-detect-virt").strip() != "oracle" or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay" or os.geteuid() == 0:
        raise SystemExit("Only run as the regular user in the disposable Garuda live VM")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        raise SystemExit("Use a fresh output directory")
    checks = None
    try:
        checks = FooterChecks(args.binary, args.output, "unix:/tmp/pacpin-ui-kitty.sock", False, False, "all")
        if args.section == "discovery":
            checks.discovery_cases()
        else:
            checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Manager footer cases failed")


if __name__ == "__main__":
    main()
