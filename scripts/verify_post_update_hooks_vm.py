#!/usr/bin/env python3
"""Real Garuda/Kitty package transactions and explicit hook/helper fixtures."""
import argparse
import json
import os
from pathlib import Path
import shlex

from verify_upgrade_ui_vm import Checks, run, transcript, serialized_verifier


class HookChecks(Checks):
    def suite(self):
        original = self.cfg.read_text()
        marker = self.root / "hook-invocations.txt"
        script = self.root / "user-hook.sh"
        script.write_text('#!/bin/sh\nprintf "%s:%s:%s\\n" "$PACPIN_HOOK" "$PACPIN_UPDATED_PACKAGES" "$1" >> ' + shlex.quote(str(marker)) + '\necho "HOOK_SCRIPT_OUTPUT_RETAINED"\nsleep 3\nexit "${2:-0}"\n')
        script.chmod(0o755)
        hook = f'\n[hooks.user]\nwatch=["pacpin-ui-native","pacpin-ui-source"]\naction="script"\ncommand=["{script}","literal;argument"]\n'
        self.cfg.write_text(original + hook)
        try:
            self.seed()
            self.publish(2)
            self.catalog(2)
            self.case("hooks-dry-run", ["-Su", "--dry-run", "--noconfirm"])
            assert not marker.exists()
            raw = transcript(self.root / "hooks-dry-run.raw.txt")
            summary = "Packages to Change" if "Packages to Change" in raw else "Packages to Upgrade"
            assert raw.index("Post-update hooks") < raw.index(summary)
            self.case("hooks-native-source-once", ["-Su", "--no-cache", "--use-storage", "--noconfirm"], native=True, source=True, width=40)
            assert marker.read_text().splitlines() == ["user:pacpin-ui-native pacpin-ui-source:literal;argument"]
            frames = json.loads((self.root / "hooks-native-source-once.frames.json").read_text())
            assert any("Hooks: Running user script" in frame for frame in frames)
            raw = transcript(self.root / "hooks-native-source-once.raw.txt")
            assert raw.index("Hook user: completed") < raw.index("Purging native")
            assert "HOOK_SCRIPT_OUTPUT_RETAINED" in (self.root / "hooks-native-source-once.screen.txt").read_text()
            self.case("hooks-current-version-no-trigger", ["-Su", "--noconfirm"])
            assert len(marker.read_text().splitlines()) == 1

            self.seed()
            self.publish(2)
            self.catalog(2)
            self.case("hooks-source-scope", ["-Su", "--sources-only", "--use-storage", "--noconfirm"], source=True)
            assert marker.read_text().splitlines()[-1] == "user:pacpin-ui-source:literal;argument"

            both = hook.replace('watch=[', 'watch_all=[')
            self.cfg.write_text(original + both)
            self.seed()
            self.publish(2)
            self.catalog(2)
            count = len(marker.read_text().splitlines())
            self.case("hooks-all-condition-one-update-skips", ["-Su", "--sources-only", "--use-storage", "--noconfirm"], source=True)
            assert len(marker.read_text().splitlines()) == count
            self.seed()
            self.publish(2)
            self.catalog(2)
            self.case("hooks-all-condition-both-update-runs", ["-Su", "--use-storage", "--noconfirm"], native=True, source=True)
            assert len(marker.read_text().splitlines()) == count + 1

            veto = hook.replace('action="script"', 'unless_updated=["pacpin-ui-native"]\naction="script"')
            self.cfg.write_text(original + veto)
            self.seed()
            self.publish(2)
            self.catalog(2)
            count = len(marker.read_text().splitlines())
            self.case("hooks-unless-condition-skips", ["-Su", "--use-storage", "--noconfirm"], native=True, source=True)
            assert len(marker.read_text().splitlines()) == count
            self.seed()
            self.publish(2)
            self.catalog(2)
            self.case("hooks-unless-condition-allows-other-update", ["-Su", "--sources-only", "--use-storage", "--noconfirm"], source=True)
            assert len(marker.read_text().splitlines()) == count + 1

            nested = self.root / "nested-hook.sh"
            nested.write_text('#!/bin/sh\n' + shlex.join([str(self.binary), "-Su", "--dry-run", "--noconfirm"]) + '\n')
            nested.chmod(0o755)
            self.cfg.write_text(original + hook.replace(str(script), str(nested)))
            self.seed()
            self.publish(2)
            self.catalog(2)
            self.case("hooks-nested-invocation-no-recursion", ["-Su", "--sources-only", "--use-storage", "--noconfirm"], source=True, expected_error="hooks skipped in a hook subprocess")

            self.seed()
            self.publish(2)
            self.catalog(2)
            self.cfg.write_text(original + hook.replace('"literal;argument"]', '"literal;argument","7"]'))
            self.case("hooks-script-failure", ["-Su", "--sources-only", "--use-storage", "--noconfirm"], source=True, success=False, source_completed=True, expected_error="Hook user: failed")
            assert run("pacman", "-Q", "pacpin-ui-source").strip().endswith("2.0.0-1")

            self.cfg.write_text(original + hook)
            self.seed()
            self.publish(1)
            self.catalog(2, valid=False)
            count = len(marker.read_text().splitlines())
            self.case("hooks-failed-source-skips-action", ["-Su", "--sources-only", "--use-storage", "--noconfirm"], source=True, success=False, expected_error="Post-update hooks skipped")
            assert len(marker.read_text().splitlines()) == count

            build = self.root / "rebuild-fixture"
            build.mkdir()
            run("sudo", "-n", "rm", "-rf", "--", "/opt/pacpin-hook-hello")
            (build / "build-marker").write_text("old\n")
            (build / "PKGBUILD").write_text("pkgname=pacpin-hook-hello\npkgver=2.0.0\npkgrel=1\narch=('x86_64')\noptions=('!strip' '!debug')\npackage() { install -Dm755 /usr/bin/true \"$pkgdir/opt/pacpin-hook-hello/app\"; install -Dm644 \"$startdir/build-marker\" \"$pkgdir/opt/pacpin-hook-hello/build-marker\"; }\n")
            run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=build)
            package = next(build.glob("*.pkg.tar.zst"))
            run("sudo", "-n", "pacman", "-U", "--noconfirm", str(package))
            helperdir = self.root / "hook-helper"
            helperdir.mkdir()
            helper = helperdir / "paru"
            args_record = self.root / "rebuild-arguments.txt"
            helper.write_text('#!/bin/sh\ncase "$1" in -Scc) echo "CONTROLLED_AUR_CACHE_CLEAN"; exit 0;; esac\nprintf "%s\\n" "$@" > ' + shlex.quote(str(args_record)) + '\ncd ' + shlex.quote(str(build)) + '\nmakepkg --force --nodeps --noconfirm --nocolor || exit $?\n: "${PKGDEST:?missing configured workspace}"\nsudo -n pacman -U --noconfirm "$PKGDEST"/pacpin-hook-hello-2.0.0-1-*.pkg.tar.zst || exit $?\nsleep 2\n')
            helper.chmod(0o755)
            rebuild = f'\n[hooks.rebuild]\nwatch=["pacpin-ui-native","pacpin-ui-source"]\naction="rebuild-aur"\npackages=["pacpin-hook-hello"]\n'
            self.cfg.write_text(original.replace('helper = "paru"', f'helper = "{helper}"') + rebuild)
            self.seed()
            self.publish(2)
            self.catalog(2)
            self.case("hooks-rebuild-source-scope-skipped", ["-Su", "--sources-only", "--use-storage", "--noconfirm"], source=True, expected_error="outside --sources-only")
            assert not args_record.exists()
            self.seed(source=False)
            self.publish(2)
            self.cfg.write_text(self.cfg.read_text() + hook)
            (build / "build-marker").write_text("rebuilt\n")
            self.case("hooks-same-version-aur-rebuild", ["-Su", "--use-storage", "--no-cache", "--noconfirm"], native=True, aur=True, aur_package="pacpin-hook-hello")
            argv = args_record.read_text().splitlines()
            assert "--rebuild" in argv and "--needed" not in argv
            assert "--clonedir" in argv and argv[argv.index("--clonedir") + 1].startswith("/var/tmp/pacpin-aur-")
            assert "reinstalling pacpin-hook-hello" in transcript(self.root / "hooks-same-version-aur-rebuild.raw.txt")
            assert Path("/opt/pacpin-hook-hello/build-marker").read_text() == "rebuilt\n"
            frames = json.loads((self.root / "hooks-same-version-aur-rebuild.frames.json").read_text())
            assert any("AUR: ✔ Upgrade complete" in frame and "Hooks:" in frame for frame in frames)
            self.results[-1]["aur_helper"] = "controlled fixture; real makepkg and pacman reinstall"

            # A helper's zero exit is insufficient when installed metadata no
            # longer has healthy dependencies. Inject only into the fixture DB.
            desc = self.root / "db/local/pacpin-hook-hello-2.0.0-1/desc"
            backup = self.root / "hello-desc.original"
            backup.write_bytes(desc.read_bytes())
            helper.write_text(helper.read_text().replace('sleep 2', 'printf "%%DEPENDS%%\\npacpin-hook-missing-dependency\\n\\n" | sudo -n tee -a ' + shlex.quote(str(desc)) + ' >/dev/null\nsleep 2'))
            self.seed(source=False)
            self.publish(2)
            count = len(marker.read_text().splitlines())
            try:
                self.case("hooks-rebuild-verification-failure", ["-Su", "--use-storage", "--no-cache", "--noconfirm"], native=True, aur=True, aur_package="pacpin-hook-hello", success=False, expected_error="pacpin-hook-missing-dependency")
                assert len(marker.read_text().splitlines()) == count
                raw = transcript(self.root / "hooks-rebuild-verification-failure.raw.txt")
                assert "AUR cache cleanup skipped: transaction failed" in raw
                argv = args_record.read_text().splitlines()
                workspace = Path(argv[argv.index("--clonedir") + 1]).parent
                assert workspace.name.startswith("pacpin-aur-") and workspace.exists()
                assert list((workspace / "packages").glob("pacpin-hook-hello-*.pkg.tar.zst"))
                frames = json.loads((self.root / "hooks-rebuild-verification-failure.frames.json").read_text())
                assert not any("AUR: ✔" in frame for frame in frames)
                self.results[-1]["injection"] = "helper exits zero after adding an unsatisfied dependency to fixture metadata"
            finally:
                run("sudo", "-n", "cp", str(backup), str(desc))
        finally:
            self.cfg.write_text(original)


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or 'ID=garuda' not in Path("/etc/os-release").read_text() or run("systemd-detect-virt").strip() != "oracle" or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay" or os.geteuid() == 0:
        raise SystemExit("Run as the regular user in the disposable Garuda live VM")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        raise SystemExit("Use a fresh output directory")
    checks = None
    try:
        checks = HookChecks(args.binary, args.output, "unix:/tmp/pacpin-ui-kitty.sock", False, False, "hooks")
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Hook terminal verification failed; inspect results.json")


if __name__ == "__main__":
    main()
