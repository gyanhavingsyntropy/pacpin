#!/usr/bin/env python3
"""Stage-aware upgrade review checks in the user-selected disposable guest.

Reuses the existing guarded Garuda/Kitty fixtures; never run on the host.
Native/Sources operations are real. External managers are controlled fixtures.
"""
import argparse
import json
import os
from pathlib import Path
import re

from verify_preparation_vm import PreparationChecks
from verify_upgrade_ui_vm import run, serialized_verifier, transcript


class ReviewChecks(PreparationChecks):
    def extras(self):
        self.original_cfg = self.cfg.read_text()
        name = "combined-native-external-cache-estimate"
        self.reset_case(name)
        self.fixture_preparation(("flatpak", "nix"))
        (self.root / "cache/estimate-fixture").write_bytes(b"x" * 4096)
        self.case(name, ["-Su", "--no-cache", "--dry-run", "--noconfirm"], width=44)
        self.verify_plan(name, {"Pacman", "Flatpak", "Nix"}, cache=True)
        text = " ".join(transcript(self.root / f"{name}.raw.txt").replace("│", " ").split())
        assert "~4.0 KiB + unknown" in text
        assert (self.root / "cache/estimate-fixture").stat().st_size == 4096
        assert not (self.root / "flatpak-actions.json").exists()
        assert not (self.root / "nix-actions.json").exists()

        name = "incomplete-native-cache-estimate"
        self.reset_case(name)
        (self.root / "cache/estimate-fixture").write_bytes(b"x" * 4096)
        nested = self.root / "cache/nested-download"
        nested.mkdir()
        (nested / "keep").write_text("preserve")
        self.case(name, ["-Su", "--no-cache", "--dry-run", "--noconfirm"])
        self.verify_plan(name, {"Pacman"}, cache=True)
        text = " ".join(transcript(self.root / f"{name}.raw.txt").split())
        assert "~4.0 KiB + unknown" in text and "Cache sizes unknown: Pacman." in text
        assert (nested / "keep").read_text() == "preserve"
        (nested / "keep").unlink()
        nested.rmdir()

        name = "compact-custom-options"
        self.reset_case(name, native=False)
        self.fixture_preparation(("flatpak", "nix"))
        self.case(name, ["-Su", "--external-only", "--flatpak.review", "--no-cache", "--noconfirm"], width=44)
        self.verify_plan(name, {"Flatpak", "Nix"}, cache=True)
        text = " ".join(transcript(self.root / f"{name}.raw.txt").split())
        assert "Flatpak option review: after success run" in text
        assert "download-only preparation:" not in text
        assert json.loads((self.root / "flatpak-actions.json").read_text()) == ["prepare", "upgrade", "action", "cache"]
        frames = json.loads((self.root / f"{name}.frames.json").read_text())
        assert not any(re.search(r"(?:^|\n)(?:Cache|Orphans): Pending", frame) for frame in frames)
        for manager in ["Flatpak", "Nix"]:
            assert any("✔ Updating" in frame and "● Cache" in frame and f"Cache: {manager}:" in frame for frame in frames)
        assert not any("● Updating" in frame and "fixture operation: cache" in frame and re.search(r"(?:^|\n)Cache:", frame) for frame in frames)

        name = "compact-cache-disabled-without-cleanup"
        self.reset_case(name, native=False, external=("nix",))
        managers = self.cfg.parent / "managers.toml"
        body = "\n".join(line for line in managers.read_text().splitlines() if not line.startswith("cache_cleanup ="))
        managers.write_text(body.replace("[managers.nix]\n", '[managers.nix]\nno_cache_args = ["--avoid-cache"]\n') + "\n")
        self.case(name, ["-Su", "--external-only", "--no-cache", "--noconfirm"])
        self.verify_plan(name, {"Nix"}, cache_estimate=True)
        assert json.loads((self.root / "nix-actions.json").read_text()) == ["upgrade"]
        frames = json.loads((self.root / f"{name}.frames.json").read_text())
        assert not any("Cache: Pending" in frame for frame in frames)

        name = "compact-native-source-live-resize"
        self.reset_case(name, source=True, orphans=True)
        # Empty live caches can finish between screen samples. Delay only this
        # guest fixture's ordinary sudo forwarding for -Scc, then execute real
        # sudo/pacman. This script has no special permissions or host effects.
        delay = self.root / "cache-delay"
        delay.mkdir()
        helper = delay / "sudo"
        helper.write_text('#!/bin/sh\ncase " $* " in *" -Scc "*) printf "PACPIN_CACHE_TEST_PAUSE\\n"; sleep 1;; *" -Rn "*) printf "PACPIN_ORPHAN_TEST_PAUSE\\n"; sleep 1;; esac\nexec /usr/bin/sudo "$@"\n')
        helper.chmod(0o755)
        original_path = self.env["PATH"]
        self.env["PATH"] = str(delay) + ":" + original_path
        self.case(name, ["-Su", "--no-cache", "--noconfirm", "--autoremove"], native=True, source=True, resize=True)
        self.env["PATH"] = original_path
        self.verify_plan(name, {"Pacman", "Sources"}, internal=True, orphan=True, cache=True)
        frames = json.loads((self.root / f"{name}.frames.json").read_text())
        assert any("● Updating" in frame and "○ Cache" in frame for frame in frames)
        assert any("● Cache" in frame for frame in frames)
        assert any("✔ Updating" in frame and "● Cache" in frame and "Cache: Native:" in frame for frame in frames)
        assert not any(re.search(r"(?:^|\n)(?:Cache|Orphans): Pending", frame) for frame in frames)
        assert not any("● Cache" in frame and "Pacman: ✔" in frame for frame in frames)
        assert any("✔ Updating" in frame and "● Orphans" in frame for frame in frames)
        assert any("✔ Updating" in frame and "✔ Orphans" in frame and "● Cache" in frame for frame in frames)

        name = "compact-redirected-dry-run"
        self.reset_case(name, native=False, source=True)
        text = run(str(self.binary), "-Su", "--sources-only", "--dry-run", "--noconfirm", env=self.env)
        assert "Actions: Sources" in text and "Internal action:" in text
        assert "\x1b" not in text
        assert run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1")
        (self.root / f"{name}.txt").write_text(text)
        self.results.append({"case": name, "status": 0, "failures": [], "capture": "redirected stdout"})
        self.checkpoint()

    def verify_plan(self, name, managers, internal=False, orphan=False, cache=False, keep=False, hooks=False, cache_estimate=None):
        result = self.results[-1]
        raw = transcript(self.root / f"{name}.raw.txt")
        text = re.sub(r"\s+", " ", raw)
        def require(value, message):
            if not value:
                result["failures"].append(message)
        require("Actions:" in text, "action labels missing")
        require("Synthesized Upgrade Command" not in text, "old combined command remains")
        require("Upgrade execution plan:" not in text, "verbose routine heading remains")
        dry = "Dry-Run: Actions:" in text
        action_match = re.search(r"(?:^|\n)(?::: |Dry-Run: )Actions: (.*?)(?:\n\s*\n|\Z)", raw, re.S)
        actions = re.sub(r"\s+", " ", action_match.group(1)) if action_match else ""
        for manager in ["Pacman", "AUR", "Sources", "Flatpak", "Nix", "Pipx"]:
            present = re.search(rf"\b{manager}\b", actions) is not None
            require(present == (manager in managers), f"{manager} stage scope mismatch")
        require(("Internal action:" in text) == (internal and dry), "internal action classification mismatch")
        if not dry:
            require("Base command:" not in text and "Preparation:" not in text, "routine approval still verbose")
        require("Restart advisory:" not in actions, "restart advice added as an approval action")
        require(re.search(r"\d+\. (?:Pacman|AUR|Sources|Flatpak|Nix) \(", text) is None, "sequential numbering remains")
        require("pacpin -S --" not in text, "Sources presented as a recursive command")
        require(("Orphan review" in actions or "Orphan cleanup (" in actions) == orphan, "orphan review scope mismatch")
        require(("Cache cleanup (on success)" in actions) == cache, "cache lifecycle scope mismatch")
        require(("Hooks (if triggered)" in actions) == hooks, "hook scope mismatch")
        if "Pacman" in managers and "Sources" in managers:
            require(actions.index("Pacman") < actions.index("Sources"), "native/source labels missing")
        if cache_estimate is None:
            cache_estimate = cache and bool(managers - {"Sources"})
        require(("Cache Before Update" in text) == cache_estimate, "combined cache estimate scope mismatch")
        require("Cache Now" not in text and "🗃" not in text, "old cache label or ambiguous icon remains")
        if cache_estimate:
            for manager in managers - {"Pacman", "Sources"}:
                require(re.search(rf"Cache sizes unknown: [^.]*\b{manager}\b", text) is not None, f"{manager} cache incorrectly appears measured/empty")
        if name.startswith("native-cache-border-"):
            # Inspect captured rendered text with Kitty's independent width
            # engine, not pacpin's own width helper tested against itself.
            code = ("import json, sys; from pathlib import Path; "
                    "from kitty.fast_data_types import wcswidth; "
                    "lines = Path(sys.argv[1]).read_text().splitlines(); "
                    "print(json.dumps([wcswidth(line) for line in lines "
                    "if line.startswith(('  ┌', '  │', '  └'))]))")
            widths = json.loads(run("kitty", "+runpy", code, str(self.root / f"{name}.review-screen.txt")))
            require(len(widths) >= 8 and len(set(widths)) == 1, f"rendered card border columns differ: {widths}")
        require("Net Space Delta" not in text, "ambiguous package/cache delta label remains")
        markers = ["Hooks:", "Orphan cleanup:", "Cache cleanup (--no-cache):", "Restart advisory:"]
        (self.root / f"{name}.lifecycle.txt").write_text("\n".join(
            line for line in raw.splitlines() if any(marker in line for marker in markers)))
        self.checkpoint()

    def capture(self, name):
        return lambda: (
            (self.root / f"{name}.review-screen.txt").write_text(self.rc("get-text", "--extent", "screen")),
            (self.root / f"{name}.review.txt").write_text(self.rc("get-text", "--extent", "all")))

    def cache_cards(self):
        self.original_cfg = self.cfg.read_text()
        for width, policy in [(80, "--use-ram"), (44, "--use-storage")]:
            name = f"native-cache-border-{width}"
            self.reset_case(name, source=False)
            self.case(name, ["-Su", "--no-cache", policy], confirm=True, confirm_response="n", confirm_action=self.capture(name), width=width)
            self.verify_plan(name, {"Pacman"}, cache=True)
            text = " ".join(transcript(self.root / f"{name}.raw.txt").split())
            assert f"{policy} has no effect here: no temporary build files are needed." in text
            assert "Scratch preference unused" not in text
            assert run("pacman", "-Q", "pacpin-ui-native").strip().endswith("1.0.0-1")

    def suite(self):
        self.cache_cards()
        name = "decline-native-source-orphans"
        self.reset_case(name, source=True, orphans=True)
        self.case(name, ["-Su", "--no-cache"], confirm=True, confirm_response="n", confirm_action=self.capture(name), width=80)
        self.verify_plan(name, {"Pacman", "Sources"}, internal=True, orphan=True, cache=True)
        assert run("pacman", "-Q", "pacpin-ui-native").strip().endswith("1.0.0-1")
        assert run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1")
        assert "Showing " not in transcript(self.root / f"{name}.raw.txt")

        name = "dry-mixed-defaults"
        self.reset_case(name, source=True, orphans=True)
        self.case(name, ["-Su", "--dry-run", "--no-cache"], width=80)
        self.verify_plan(name, {"Pacman", "Sources"}, internal=True, orphan=True, cache=True)
        assert run("pacman", "-Q", "pacpin-ui-native").strip().endswith("1.0.0-1")

        name = "native-source-review-narrow"
        self.reset_case(name, source=True, orphans=True)
        self.case(name, ["-Su", "--no-cache", "--use-storage"], native=True, source=True,
                  confirm=True, confirm_action=self.capture(name), orphan_choice=[], width=44)
        self.verify_plan(name, {"Pacman", "Sources"}, internal=True, orphan=True, cache=True)
        text = re.sub(r"\s+", " ", transcript(self.root / f"{name}.raw.txt"))
        frames = json.loads((self.root / f"{name}.frames.json").read_text())
        assert not any(re.search(r"(?:^|\n)(?:Cache|Orphans): Pending", frame) for frame in frames)
        assert text.index("Actions:") < text.index("Proceed with installation?") < text.index("CLEAN ORPHANED PACKAGES")
        assert "Scratch policy" in text or "Storage" in text

        name = "source-only-keep-going"
        self.reset_case(name, native=False, source=True)
        self.case(name, ["-Su", "--sources-only", "--no-cache", "--keep-going", "--noconfirm"], source=True)
        self.verify_plan(name, {"Sources"}, internal=True, keep=True)

        name = "native-noconfirm-no-autoremove"
        self.reset_case(name, source=False, orphans=True)
        self.case(name, ["-Su", "--noconfirm"], native=True)
        self.verify_plan(name, {"Pacman"})
        assert "selected for removal" not in transcript(self.root / f"{name}.raw.txt")

        name = "external-flatpak-nix-narrow"
        self.reset_case(name, native=False)
        self.fixture_preparation(("flatpak", "nix"))
        self.case(name, ["-Su", "--external-only", "--no-cache", "--noconfirm"], width=44)
        self.verify_plan(name, {"Flatpak", "Nix"}, cache=True)
        for manager in ["flatpak", "nix"]:
            assert json.loads((self.root / f"{manager}-actions.json").read_text()) == ["prepare", "upgrade", "cache"]

        name = "external-dry-run"
        self.reset_case(name, native=False)
        self.fixture_preparation(("flatpak", "nix"))
        self.case(name, ["-Su", "--external-only", "--dry-run", "--no-cache", "--noconfirm"])
        self.verify_plan(name, {"Flatpak", "Nix"}, cache=True)
        for manager in ["flatpak", "nix"]:
            assert not (self.root / f"{manager}-actions.json").exists()

        name = "unsupported-external-keep-going"
        self.reset_case(name, external=("flatpak",))
        self.case(name, ["-Su", "--use-storage", "--keep-going", "--no-cache", "--noconfirm"], native=True, success=False)
        self.verify_plan(name, {"Pacman"}, cache=True, keep=True)
        assert not (self.root / "flatpak-actions.json").exists()
        assert run("pacman", "-Q", "pacpin-ui-native").strip().endswith("2.0.0-1")

        name = "no-work"
        self.reset_case(name, native=False)
        self.case(name, ["-Su", "--noconfirm"])
        assert "Actions:" not in transcript(self.root / f"{name}.raw.txt")
        self.checkpoint()


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--section", choices=["review", "extras", "cache-cards"], default="review")
    args = parser.parse_args()
    if (os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or os.geteuid() == 0
            or "ID=garuda" not in Path("/etc/os-release").read_text()
            or run("systemd-detect-virt").strip() != "oracle"
            or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay"):
        raise SystemExit("Requires the selected disposable Garuda/VirtualBox live guest as a regular user")
    args.output.mkdir(parents=True, exist_ok=False)
    checks = None
    try:
        checks = ReviewChecks(args.binary, args.output, "unix:/tmp/pacpin-ui-kitty.sock", False, False, "review")
        if args.section == "extras":
            checks.extras()
        elif args.section == "cache-cards":
            checks.cache_cards()
        else:
            checks.suite()
    finally:
        if checks:
            checks.checkpoint()
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    failures = sum(bool(result["failures"]) for result in checks.results)
    print(json.dumps({"cases": len(checks.results), "failures": failures}))
    if failures:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
