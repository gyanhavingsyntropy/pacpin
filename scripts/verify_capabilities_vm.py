#!/usr/bin/env python3
"""Capability/continuation contracts in the disposable guest's real Kitty terminal.

Native and Sources transactions are real. External managers are controlled
literal-command fixtures, not actual Flatpak/Nix/Pipx application upgrades.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shutil

from verify_upgrade_ui_vm import Checks, run, transcript, serialized_verifier


class CapabilityChecks(Checks):
    def configure(self, names=("flatpak",), cleanup=True, empty=False, invalid=False):
        definitions = ["version = 1"]
        for name in names:
            helper = self.root / f"{name}-fixture"
            record = self.root / f"{name}-actions.json"
            record.unlink(missing_ok=True)
            helper.write_text("#!/usr/bin/python\n" + "\n".join([
                "import json, sys, time",
                "from pathlib import Path",
                f"record = Path({str(record)!r})",
                "actions = json.loads(record.read_text()) if record.exists() else []",
                "operation = sys.argv[1]",
                "if operation == 'check':",
                "    print(json.dumps([] if " + repr(empty) + " or 'upgrade' in actions else [{'id': 'org.pacpin.Test', 'name': 'capability-fixture', 'version': '2.0', 'installed_version': '1.0', 'repo': 'fixture'}]))",
                "elif operation == 'verify':",
                "    sys.exit(0 if 'upgrade' in actions else 1)",
                "else:",
                "    actions.append(operation)",
                "    record.write_text(json.dumps(actions))",
                "    print('fixture operation: ' + operation, flush=True)",
                "    time.sleep(1)",
            ]) + "\n")
            helper.chmod(0o755)
            upgrade = str(self.root / "missing-upgrade-executable") if invalid else str(helper)
            definitions.extend([
                f"[managers.{name}]",
                f"adapter = {json.dumps(name if name in ['flatpak', 'nix', 'pipx'] else 'command')}",
                "enabled = true",
                f"check = {json.dumps([str(helper), 'check'])}",
                f"upgrade = {json.dumps([upgrade, 'upgrade', '{id}', '{version}'])}",
                f"verify = {json.dumps([str(helper), 'verify', '{id}', '{version}'])}",
            ])
            if cleanup:
                definitions.append(f"cache_cleanup = {json.dumps([str(helper), 'cache'])}")
            if name == "custom":
                definitions.append(f"upgrade_storage = {json.dumps([upgrade, 'upgrade', '{id}', '{version}'])}")
                definitions.append("scratch_mib = 1")
            definitions.extend([
                f"[managers.{name}.flags.review]",
                f"command = {json.dumps([str(helper), 'action'])}",
            ])
        (self.cfg.parent / "managers.toml").write_text("\n".join(definitions) + "\n")

    def check(self, name, args, native_updated=False, source_updated=False, success=False, expected=None, width=80, skipped=(), no_effect=(), dry=False, aur=False, status=None):
        self.case(name, args, native=native_updated, source=source_updated,
                  success=success, source_completed=source_updated and not success,
                  expected_error=expected, width=width, orphans=False, cache=None, aur=aur)
        result = self.results[-1]
        text = transcript(self.root / f"{name}.raw.txt")
        frames = json.loads((self.root / f"{name}.frames.json").read_text())
        def require(condition, message):
            if not condition:
                result["failures"].append(message)
        require(result["status"] == (status if status is not None else 0 if success else 2), "unexpected complete/incomplete status")
        require(run("pacman", "-Q", "pacpin-ui-native").strip().endswith("2.0.0-1" if native_updated and not dry else "1.0.0-1"), "native result differs from eligible plan")
        require(run("pacman", "-Q", "pacpin-ui-source").strip().endswith("2.0.0-1" if source_updated and not dry else "1.0.0-1"), "Sources result differs from eligible plan")
        for manager in skipped:
            require(f"{manager} skipped (--keep-going" in text, f"{manager} skip not previewed")
            if native_updated and not dry:
                require(any(f"{manager}: ⚠ Skipped" in frame and "Pacman:" in frame for frame in frames), f"{manager} skipped footer missing during native work")
            require(f"✔ {manager} upgrade completed" not in text, f"{manager} falsely completed")
        for manager in no_effect:
            require(not (self.root / f"{manager}-actions.json").exists(), f"{manager} executed upgrade/cache despite exclusion")
        if skipped:
            require("Upgrade incomplete:" in text, "incomplete result not explained")
            require("All packages are up to date" not in text, "skipped updates reported as up-to-date")
        if native_updated and not dry:
            scrollback = (self.root / f"{name}.screen.txt").read_text()
            for index in range(1, 41):
                require(f"PACPIN_NATIVE_HISTORY_{index:02}" in scrollback, "native history lost")
        if "--no-cache" in args and native_updated and not dry:
            require("Native cache cleaned successfully" in text, "successful native cache not cleaned")
        result["external_managers"] = "controlled fixtures"

    def suite(self):
        if self.section == "identities":
            self.identity_cases()
            return
        cases = [
            ("strict-ram-unsupported-flatpak", ["-Su", "--use-ram", "--noconfirm"], dict(expected="does not support Ram", no_effect=("flatpak",))),
            ("ram-native-flatpak-keep-going", ["-Su", "--use-ram", "--keep-going", "--no-cache", "--noconfirm"], dict(native_updated=True, skipped=("Flatpak",), no_effect=("flatpak",), width=40)),
            ("storage-native-sources-flatpak-keep-going", ["-Su", "--use-storage", "--keep-going", "--no-cache", "--noconfirm"], dict(native_updated=True, source_updated=True, skipped=("Flatpak",), no_effect=("flatpak",), width=40)),
            ("cache-native-flatpak-keep-going", ["-Su", "--keep-going", "--no-cache", "--noconfirm"], dict(native_updated=True, skipped=("Flatpak",), no_effect=("flatpak",))),
            ("multiple-unsupported-managers", ["-Su", "--use-ram", "--keep-going", "--noconfirm"], dict(native_updated=True, skipped=("Flatpak", "Nix", "Pipx", "custom"), no_effect=("flatpak", "nix", "pipx", "custom"), width=40)),
            ("skipped-manager-custom-action", ["-Su", "--use-ram", "--keep-going", "--flatpak.review", "--noconfirm"], dict(native_updated=True, skipped=("Flatpak",), no_effect=("flatpak",))),
            ("eligible-custom-manager-and-sources", ["-Su", "--use-storage", "--keep-going", "--no-cache", "--noconfirm"], dict(native_updated=True, source_updated=True, skipped=("Flatpak",), no_effect=("flatpak",))),
            ("all-managers-unsupported", ["-Su", "--external-only", "--use-ram", "--keep-going", "--no-cache", "--noconfirm"], dict(skipped=("Flatpak",), no_effect=("flatpak",), expected="No eligible package updates remain")),
            ("dry-run-skips-before-approval", ["-Su", "--use-ram", "--keep-going", "--no-cache", "--dry-run"], dict(dry=True, skipped=("Flatpak",), no_effect=("flatpak",))),
            ("no-pending-external-work", ["-Su", "--use-ram", "--no-cache", "--noconfirm"], dict(native_updated=True, success=True, no_effect=("flatpak",))),
            ("invalid-executable-is-fatal", ["-Su", "--keep-going", "--noconfirm"], dict(expected="is unavailable", no_effect=("flatpak",))),
        ]
        for name, args, assertions in cases:
            names = ("flatpak", "nix", "pipx", "custom") if name.startswith("multiple") else ("flatpak", "custom") if name.startswith("eligible-custom") else ("flatpak",)
            self.configure(names=names,
                           cleanup=not name.startswith("cache-native"),
                           empty=name.startswith("no-pending"), invalid=name.startswith("invalid"))
            self.seed()
            self.publish(2)
            self.catalog(2 if assertions.get("source_updated") else 1)
            original_cfg = self.cfg.read_text()
            pending = self.root / "state/pacpin/pending_orphan_cleanup.json"
            pending.parent.mkdir(parents=True, exist_ok=True)
            pending.write_text('["pacpin-ui-preserved-orphan"]\n')
            if assertions.get("skipped"):
                self.cfg.write_text(original_cfg.replace("smart_orphans = false", "smart_orphans = true"))
            self.check(name, args, **assertions)
            if assertions.get("skipped") and pending.read_text() != '["pacpin-ui-preserved-orphan"]\n':
                self.results[-1]["failures"].append("pending orphan cleanup was acknowledged despite skipped work")
            self.cfg.write_text(original_cfg)
            if name.startswith("eligible-custom"):
                actions = json.loads((self.root / "custom-actions.json").read_text())
                if actions != ["upgrade", "cache"]:
                    self.results[-1]["failures"].append("eligible custom manager did not upgrade and clean exactly once")
        self.aur_capability_cases()

    def identity_cases(self):
        for name, expected in [
            ("core-manager-key-cannot-change-adapter", "reserved manager key must use adapter"),
            ("external-label-cannot-shadow-flatpak", "external manager display name is reserved"),
        ]:
            self.configure()
            self.seed()
            self.publish(2)
            self.catalog(1)
            if name.startswith("core-"):
                managers = self.cfg.parent / "managers.toml"
                managers.write_text(managers.read_text().replace('adapter = "flatpak"', 'adapter = "command"'))
            else:
                self.configure(names=("flatpak", "Flatpak"))
            self.check(name, ["-Su", "--keep-going", "--noconfirm"], expected=expected, no_effect=("flatpak",), status=1)

    def aur_capability_cases(self):
        original_cfg = self.cfg.read_text()
        helper = self.root / "unsupported-aur-helper"
        marker = self.root / "unsupported-aur-invoked"
        helper.write_text(f"#!/bin/sh\ntouch {marker}\nexit 99\n")
        helper.chmod(0o755)
        self.cfg.write_text(original_cfg.replace('helper = "paru"', f'helper = "{helper}"').replace('[pins]', '[pins]\nhello = "aur"'))
        package = self.root / "db/local/hello-0.0.0-1"
        package.mkdir()
        (package / "desc").write_text("%NAME%\nhello\n\n%VERSION%\n0.0.0-1\n\n%REASON%\n0\n\n%SIZE%\n1\n\n%ARCH%\nx86_64\n\n")
        (package / "files").write_text("%FILES%\n\n")
        try:
            self.configure(empty=True)
            self.seed()
            self.publish(2)
            self.catalog(2)
            self.check("unsupported-aur-keeps-independent-managers", ["-Su", "--use-storage", "--keep-going", "--no-cache", "--noconfirm"], native_updated=True, source_updated=True, skipped=("AUR",), aur=True)
            if marker.exists():
                self.results[-1]["failures"].append("unsupported helper was invoked")
            self.seed()
            self.publish(2)
            self.catalog(1)
            self.check("invalid-aur-flags-stay-fatal", ["-Su", "--use-storage", "--keep-going", "--clonedir=/var/tmp/private", "--noconfirm"], expected="Scratch policy conflicts")
            if marker.exists():
                self.results[-1]["failures"].append("invalid helper options reached execution")
            self.configure(names=("AUR",), cleanup=False)
            controlled = self.root / "controlled-core-aur"
            controlled.mkdir()
            paru = controlled / "paru"
            core_invoked = self.root / "core-aur-invoked"
            paru.write_text(f"#!/bin/sh\ntouch {core_invoked}\necho 'Controlled AUR helper failure'\nexit 7\n")
            paru.chmod(0o755)
            self.cfg.write_text(original_cfg.replace('helper = "paru"', f'helper = "{paru}"').replace('[pins]', '[pins]\nhello = "aur"'))
            self.seed()
            self.publish(2)
            self.catalog(1)
            self.check("external-namesake-cannot-skip-core-aur", ["-Su", "--keep-going", "--no-cache", "--noconfirm"], native_updated=True, skipped=("AUR",), no_effect=("AUR",), aur=True, status=7)
            if not core_invoked.exists():
                self.results[-1]["failures"].append("external namesake prevented the core AUR stage from executing")
        finally:
            self.cfg.write_text(original_cfg)
            shutil.rmtree(package)


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--socket", default="unix:/tmp/pacpin-ui-kitty.sock")
    parser.add_argument("--section", choices=["all", "identities"], default="all")
    args = parser.parse_args()
    if os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or 'ID=garuda' not in Path("/etc/os-release").read_text() or run("systemd-detect-virt").strip() != "oracle" or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay":
        raise SystemExit("Only run in the disposable Garuda live VM")
    if os.geteuid() == 0:
        raise SystemExit("Run as the regular live user")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        raise SystemExit("Use a fresh output directory")
    checks = None
    try:
        checks = CapabilityChecks(args.binary, args.output, args.socket, False, False, args.section)
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Capability scenarios failed; inspect results.json")


if __name__ == "__main__":
    main()
