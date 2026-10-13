#!/usr/bin/env python3
"""User manager configuration regressions, only in disposable Garuda/Kitty.

Command adapters below are controlled fixtures. Real-manager transactions are
separate from these fixtures and must be reported separately in evidence.
"""
import argparse
import json
import os
import re
from pathlib import Path
import subprocess

from verify_upgrade_ui_vm import Checks, run, serialized_verifier, transcript


class ManagerChecks(Checks):
    def definitions(self, names=("demo",), scratch=True, cache=True):
        result = "version = 1\n"
        for name in names:
            command = ["/usr/bin/python", str(self.fixture), "upgrade", name, "--", "{id}", "{version}"]
            result += f'\n[managers.{name}]\nadapter = "command"\nenabled = true\n'
            for mode in ["check", "verify"]:
                args = ["/usr/bin/python", str(self.fixture), mode, name]
                if mode == "verify":
                    args += ["--", "{id}", "{version}"]
                result += f"{mode} = {json.dumps(args)}\n"
            result += f"upgrade = {json.dumps(command)}\n"
            result += f'refresh = {json.dumps(["/usr/bin/python", str(self.fixture), "refresh", name])}\n'
            if scratch:
                for mode in ["ram", "storage"]:
                    result += f"upgrade_{mode} = {json.dumps(command)}\n"
                result += "scratch_mib = 1\n"
            if cache:
                result += 'no_cache_args = ["--avoid-cache"]\n'
                result += f'cache_cleanup = {json.dumps(["/usr/bin/python", str(self.fixture), "cache", name])}\n'
            result += 'env = { PACPIN_MANAGER_TEST = "same-context" }\n'
            result += f'\n[managers.{name}.flags.verbose]\nargs = ["--verbose"]\n'
            result += f'\n[managers.{name}.flags.note]\ncommand = {json.dumps(["/usr/bin/python", str(self.fixture), "action", name])}\n'
        self.managers.write_text(result)

    def events(self, name=None, mode=None):
        values = [json.loads(line) for line in self.event_log.read_text().splitlines()] if self.event_log.exists() else []
        return [v for v in values if (name is None or v["name"] == name) and (mode is None or v["mode"] == mode)]

    def reset(self, names=("demo",), **options):
        self.event_log.unlink(missing_ok=True)
        for path in self.root.glob("manager-*"):
            if path.is_file():
                path.unlink()
        for name in names:
            (self.root / f"manager-{name}.version").write_text("1")
        self.definitions(names, **options)

    def external(self, name, flags=(), **kwargs):
        self.case(name, ["-Su", "--external-only", "--noconfirm", *flags], **kwargs)

    def suite(self):
        self.seed(source=False, native=False)
        self.publish(1)
        original_config_home = self.env["XDG_CONFIG_HOME"]
        empty_config_home = self.root / "empty-config-home"
        self.env["XDG_CONFIG_HOME"] = str(empty_config_home)
        self.case("validate-before-init-does-not-write", ["config", "validate"])
        self.case("check-before-init-does-not-write", ["check", "--repo", "-q"])
        assert not empty_config_home.exists()
        self.env["XDG_CONFIG_HOME"] = original_config_home
        self.managers = self.cfg.with_name("managers.toml")
        self.event_log = self.root / "events.jsonl"
        self.fixture = self.root / "adapter.py"
        self.fixture.write_text('''import json, os, sys, time
from pathlib import Path
root = Path(__file__).parent
mode, name = sys.argv[1:3]
with (root / "events.jsonl").open("a") as out:
    out.write(json.dumps(dict(mode=mode, name=name, args=sys.argv[3:], uid=os.geteuid(),
         tmp=os.environ.get("TMPDIR"), tag=os.environ.get("PACPIN_MANAGER_TEST"))) + "\\n")
if (root / f"manager-{name}-{mode}.fail").exists():
    print("INJECTED_MANAGER_FAILURE", file=sys.stderr); sys.exit(9)
state = root / f"manager-{name}.version"
if mode == "check":
    if (root / f"manager-{name}-malformed.fail").exists(): print("not-json"); sys.exit(0)
    identity = (root / f"manager-{name}.id").read_text() if (root / f"manager-{name}.id").exists() else "fixture"
    print(json.dumps([] if state.read_text() == "2" else [dict(id=identity, name="Fixture", repo="local", installed_version=state.read_text(), version="2")]))
elif mode in ("upgrade", "verify"):
    args = sys.argv[sys.argv.index("--") + 1:]
    if mode == "upgrade": print("MANAGER_UPGRADE_FIXTURE", flush=True); time.sleep(1); state.write_text(args[1])
    else: sys.exit(0 if state.read_text() == args[1] else 8)
else: print("MANAGER_ACTION_FIXTURE", flush=True); time.sleep(0.6)
''')
        self.reset()
        before = self.managers.read_bytes()
        self.case("config-init-preserves-manager", ["config", "init"])
        assert self.managers.read_bytes() == before
        assert self.cfg.with_name("sources.toml").exists() and self.cfg.with_name("hooks.toml").exists()
        self.case("config-validate-supplements", ["config", "validate"])
        self.external("custom-preview", ["--dry-run", "--use-storage", "--no-cache", "--demo.verbose", "--demo.note"])
        assert not self.events(mode="upgrade") and not self.events(mode="action")
        raw = transcript(self.root / "custom-preview.raw.txt")
        assert "Storage temporary workspace for demo" in raw and "Scratch preference unused" not in raw
        assert "--verbose" in raw and "--avoid-cache" in raw
        assert "cache" in raw and "adapter.py" in raw
        self.external("custom-storage-cache-and-flags", ["--use-storage", "--no-cache", "--demo.verbose", "--demo.note"], width=40)
        event = self.events(mode="upgrade")[0]
        assert event["uid"] == os.geteuid() and event["tag"] == "same-context"
        assert event["args"] == ["--avoid-cache", "--verbose", "--", "fixture", "2"]
        assert event["tmp"] and not Path(event["tmp"]).exists()
        assert self.events(mode="verify")[0]["tag"] == "same-context"
        assert len(self.events(mode="cache")) == len(self.events(mode="action")) == 1
        frames = (self.root / "custom-storage-cache-and-flags.frames.json").read_text()
        assert "demo:" in frames
        self.external("custom-no-work-skips-action", ["--demo.note"])
        assert len(self.events(mode="action")) == 1
        self.case("standalone-action-preview", ["manager", "demo", "--note", "--dry-run"])
        assert len(self.events(mode="action")) == 1
        self.case("standalone-action", ["manager", "demo", "--note", "--noconfirm"])
        assert len(self.events(mode="action")) == 2
        self.case("external-rollback-refused", ["rollback", "--dry-run"], success=False, expected_error="external-manager updates")
        self.reset()
        (self.root / "manager-demo.id").write_text("literal{version};$(touch sentinel)")
        self.external("literal-identity")
        assert self.events(mode="upgrade")[0]["args"][-2] == "literal{version};$(touch sentinel)"
        assert not Path("sentinel").exists()
        self.reset(scratch=False)
        self.external("unsupported-storage", ["--use-storage"], success=False, expected_error="does not support Storage scratch")
        assert not self.events(mode="upgrade")
        self.reset()
        available = int(re.search(r"MemAvailable:\s+(\d+)", Path("/proc/meminfo").read_text())[1]) * 1024
        if available < 5 * 1024**3 // 2 + 1024**2:
            self.external("forced-ram-keeps-reserve", ["--use-ram"], success=False, expected_error="Insufficient scratch capacity")
            assert not self.events(mode="upgrade")
        else:
            self.external("forced-ram-workspace", ["--use-ram"])
            assert self.events(mode="upgrade")[0]["tmp"]
        self.reset(cache=False)
        self.external("unsupported-cache", ["--no-cache"], success=False, expected_error="does not support --no-cache")
        assert not self.events(mode="upgrade")
        for mode, error in [("check", "INJECTED_MANAGER_FAILURE"), ("malformed", "expected")]:
            self.reset()
            (self.root / f"manager-demo-{mode}.fail").touch()
            self.external(f"failed-{mode}-not-up-to-date", success=False, expected_error=error)
            assert not self.events(mode="upgrade")
            assert "Everything is up-to-date" not in transcript(self.root / f"failed-{mode}-not-up-to-date.raw.txt")
            assert "No updates found" not in transcript(self.root / f"failed-{mode}-not-up-to-date.raw.txt")
        self.reset(("aaa", "zzz"))
        (self.root / "manager-zzz-upgrade.fail").touch()
        self.external("successful-cache-before-later-failure", ["--no-cache"], success=False, expected_error="zzz upgrade exited with errors")
        assert self.events("aaa", "cache") and not self.events("zzz", "cache")
        self.reset(("aaa", "zzz"))
        (self.root / "manager-aaa-upgrade.fail").touch()
        self.external("failed-manager-skips-later-provider", ["--no-cache"], success=False, expected_error="aaa upgrade exited with errors")
        assert not self.events("zzz", "upgrade") and not self.events(mode="cache")
        self.reset()
        (self.root / "manager-demo-cache.fail").touch()
        self.external("cache-failure-reported", ["--no-cache"], success=False, expected_error="cache cleanup failed")
        assert (self.root / "manager-demo.version").read_text() == "2"
        self.reset()
        self.external("unknown-flag-refused", ["--demo.unknown"], success=False, expected_error="Unknown")
        self.case("out-of-scope-flag-refused", ["-Su", "--sources-only", "--demo.note", "--noconfirm"], success=False, expected_error="scope")
        self.managers.write_text(before.decode() + '\n[managers.demo.mistyped]\nvalue = true\n')
        self.case("unknown-schema-field", ["config", "validate"], success=False, expected_error="unknown field")
        self.reset()
        root_check = subprocess.run(["sudo", "-n", "env", f"XDG_CONFIG_HOME={self.env['XDG_CONFIG_HOME']}", str(self.binary), "-Syu", "--external-only", "--noconfirm"], text=True, capture_output=True)
        assert root_check.returncode and "regular user" in root_check.stderr
        assert not self.events(mode="refresh") and not self.events(mode="upgrade")
        (self.root / "root-refresh-refused.log").write_text(root_check.stdout + root_check.stderr)
        self.results.append({"case": "root-refresh-refused", "status": root_check.returncode, "failures": []})
        self.case("external-only-refresh", ["-Syu", "--external-only", "--noconfirm"])
        assert len(self.events(mode="refresh")) == 1
        self.reset()
        (self.root / "manager-demo-refresh.fail").touch()
        self.case("external-refresh-failure", ["-Syu", "--external-only", "--noconfirm"], success=False, expected_error="External refresh failed")
        assert not self.events(mode="upgrade")
        self.reset()
        self.seed()
        self.publish(1)
        self.catalog(2)
        current = self.cfg.read_text()
        sections = re.split(r"(?m)(?=^\[)", current)
        source = "".join(section for section in sections if section.startswith("[sources.ui-local"))
        self.cfg.write_text("".join(section for section in sections if not section.startswith("[sources.ui-local")))
        recipe = self.cfg.with_name("sources.toml")
        recipe.write_text(source)
        hooks = self.cfg.with_name("hooks.toml")
        marker = self.root / "supplemental-hook-ran"
        hooks.write_text(f'[hooks.source-action]\nwatch = ["pacpin-ui-source"]\naction = "script"\ncommand = ["/usr/bin/touch", {json.dumps(str(marker))}]\n')
        recipe_bytes, hook_bytes = recipe.read_bytes(), hooks.read_bytes()
        self.case("supplemental-source-and-hook", ["-Su", "--sources-only", "--noconfirm"], source=True)
        assert marker.exists() and recipe.read_bytes() == recipe_bytes and hooks.read_bytes() == hook_bytes
        assert "[sources.ui-local]" not in self.cfg.read_text() and "[hooks.source-action]" not in self.cfg.read_text()
        self.cfg.write_text(self.cfg.read_text() + "\n" + recipe.read_text())
        self.case("duplicate-source-rejected", ["config", "validate"], success=False, expected_error="both")


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
        checks = ManagerChecks(args.binary, args.output, args.socket, False, False, "all")
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Manager scenarios failed; inspect rendered evidence")


if __name__ == "__main__":
    main()
