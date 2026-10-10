#!/usr/bin/env python3
"""Disposable Garuda/Kitty checks. Imports the existing UI fixture runner.

Native and Sources installs are real; AUR failure injection is a controlled
helper fixture, explicitly distinct from a real Antigravity build.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess

from verify_upgrade_ui_vm import Checks, run, transcript, serialized_verifier


class RuntimeChecks(Checks):
    def suite(self):
        if self.section != "continuation":
            self.scope_cases()
        if self.section != "scopes":
            self.continuation_cases()

    def scope_cases(self):
        self.seed()
        self.publish(2)
        self.catalog(2)
        self.case("sources-scope-storage-cache", ["-Syu", "--sources-only", "--use-storage", "--no-cache", "--noconfirm"], source=True, width=40)
        assert run("pacman", "-Q", "pacpin-ui-native").strip().endswith("1.0.0-1")
        raw = transcript(self.root / "sources-scope-storage-cache.raw.txt")
        assert "Sources temporary workspace: /var/tmp/" in raw
        assert "AUR upgrade completed" not in raw and "Purging native" not in raw

        self.seed()
        self.publish(2)
        self.catalog(2)
        storage = Path("/var/tmp") / (self.root.name + " disk scratch with spaces")
        storage.mkdir()
        self.env["TMPDIR"] = str(storage)
        try:
            self.case("sources-custom-storage-path", ["-Su", "--sources-only", "--use-storage", "--noconfirm"], source=True)
            raw = transcript(self.root / "sources-custom-storage-path.raw.txt")
            assert f"Sources temporary workspace: {storage}/pacpin-source-" in raw
            assert not list(storage.iterdir())
        finally:
            del self.env["TMPDIR"]
            storage.rmdir()

        self.seed()
        self.publish(2)
        self.catalog(2)
        available = int(next(line.split()[1] for line in Path("/proc/meminfo").read_text().splitlines() if line.startswith("MemAvailable:"))) * 1024
        self.case("sources-scope-forced-ram", ["-Su", "--sources-only", "--use-ram", "--noconfirm"], source=True, allow_ram_refusal=True)
        raw = transcript(self.root / "sources-scope-forced-ram.raw.txt")
        assert "Sources temporary workspace: /var/tmp/" not in raw
        self.results[-1]["ram_available_bytes"] = available
        self.results[-1]["positive_ram_case"] = self.results[-1]["status"] == 0
        if self.results[-1]["status"] != 0:
            assert "RAM" in raw and "OS reserve" in raw
            assert "Sources upgrade completed successfully" not in raw
            assert run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1")

        for name, args, error in [
            ("conflicting-scopes", ["-Syu", "--sources-only", "--aur-only"], "mutually exclusive"),
            ("conflicting-media", ["-Syu", "--use-ram", "--use-storage"], "mutually exclusive"),
            ("unsupported-command", ["orphans", "--use-ram"], "only supported"),
            ("wrong-manager-target", ["-Su", "--sources-only", "pacpin-ui-native", "--noconfirm"], "selected upgrade scope"),
        ]:
            self.case(name, args, success=False, expected_error=error)

        self.seed()
        self.publish(2)
        self.catalog(2)
        self.case("native-sources-keep-going-no-aur", ["-Su", "--keep-going", "--use-storage", "--no-cache", "--noconfirm"], native=True, source=True, width=40)

    def continuation_cases(self):
        helpers = self.root / "controlled-aur"
        helpers.mkdir()
        helper = helpers / "paru"
        original_cfg = self.cfg.read_text()
        self.cfg.write_text(original_cfg.replace('helper = "paru"', f'helper = "{helper}"').replace('[pins]', '[pins]\nhello = "aur"'))
        package = self.root / "db/local/hello-0.0.0-1"
        package.mkdir()
        (package / "desc").write_text("%NAME%\nhello\n\n%VERSION%\n0.0.0-1\n\n%REASON%\n0\n\n%SIZE%\n1\n\n%ARCH%\nx86_64\n\n")
        (package / "files").write_text("%FILES%\n\n")
        log = run("pacman-conf", "LogFile").strip()
        workspace_record = self.root / "aur-workspace.txt"
        try:
            for name, keep, native, policy, injection, expected_source in [
                ("aur-build-failure-default-stop", False, False, None, "build", False),
                ("aur-build-failure-keep-going", True, False, None, "build", True),
                ("native-aur-build-failure-keep-going", True, True, "--use-storage", "build", True),
                ("aur-unknown-failure-stop", True, False, None, "unknown", False),
                ("aur-native-marker-stop", True, False, None, "native-marker", False),
                ("aur-log-change-stop", True, False, None, "log-change", False),
                ("aur-signal-stop", True, False, None, "signal", False),
                ("aur-scope-storage-failure", True, False, "--use-storage", "build", False),
            ]:
                self.seed()
                scoped = name.startswith("aur-scope-")
                self.publish(2 if native or scoped else 1)
                self.catalog(2)
                workspace_record.unlink(missing_ok=True)
                code = '#!/bin/sh\necho "==> Building controlled fixture"\nsleep 5\n'
                code += f'printf "%s\\n" "${{BUILDDIR-}}" > {shlex.quote(str(workspace_record))}\n'
                if injection == "unknown":
                    code += 'echo "error: No space left on device"; exit 10\n'
                elif injection == "signal":
                    code += 'kill -TERM $$\n'
                else:
                    if injection == "native-marker":
                        code += 'echo "error: could not extract /usr/lib/fixture.so (Write failed)"\n'
                    if injection == "log-change":
                        code += f'printf "[fixture] native invocation\\n" | sudo -n tee -a {shlex.quote(log)} >/dev/null\n'
                    code += 'echo "==> ERROR: A failure occurred in build()."; exit 10\n'
                helper.write_text(code)
                helper.chmod(0o755)
                args = ["-Su", "--noconfirm", "--no-cache"]
                if keep:
                    args.append("--keep-going")
                if policy:
                    args.append(policy)
                if scoped:
                    args.append("--aur-only")
                self.case(name, args, native=native, source=not scoped, success=False, aur=True, width=40)
                raw = transcript(self.root / f"{name}.raw.txt")
                assert "hello" in raw and workspace_record.exists(), "controlled AUR target was not invoked"
                version = run("pacman", "-Q", "pacpin-ui-source").strip()
                assert version.endswith("2.0.0-1" if expected_source else "1.0.0-1"), raw
                assert "AUR: ✔" not in raw
                if expected_source:
                    assert "1 completed, 0 not completed" in raw
                if native:
                    assert run("pacman", "-Q", "pacpin-ui-native").strip().endswith("2.0.0-1")
                    assert "Native cache cleaned successfully" in raw
                if scoped:
                    assert run("pacman", "-Q", "pacpin-ui-native").strip().endswith("1.0.0-1")
                    assert "Sources after AUR failure" not in raw
                if policy:
                    builddir = Path(workspace_record.read_text().strip())
                    assert str(builddir).startswith("/var/tmp/pacpin-aur-") and builddir.exists()
                    assert "AUR cache cleanup skipped: transaction failed" in raw
                    assert "cached packages preserved for recovery" in raw
                    # Remove only this fixture's explicitly recorded private workspace.
                    assert builddir.name == "build" and builddir.parent.name.startswith("pacpin-aur-")
                    shutil.rmtree(builddir.parent)
                self.results[-1]["controlled_aur_failure"] = injection
                self.results[-1]["source_completed_after_aur_failure"] = expected_source
        finally:
            self.cfg.write_text(original_cfg)
            shutil.rmtree(package)


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--socket", default="unix:/tmp/pacpin-ui-kitty.sock")
    parser.add_argument("--section", choices=["all", "scopes", "continuation"], default="all")
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
        checks = RuntimeChecks(args.binary, args.output, args.socket, False, False, args.section)
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Runtime UI cases failed; inspect results.json")


if __name__ == "__main__":
    main()
