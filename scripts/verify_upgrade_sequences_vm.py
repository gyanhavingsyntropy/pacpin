#!/usr/bin/env python3
"""Disposable VM direct-terminal upgrade handoff and sequence audit."""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import shutil
import time

from verify_capabilities_vm import CapabilityChecks
from verify_upgrade_ui_vm import SlowRepository, run, serialized_verifier, transcript


class SequenceChecks(CapabilityChecks):
    direct_terminal = True

    def checkpoint(self):
        (self.root / "results.json").write_text(json.dumps(self.results, indent=2))

    def inventory(self):
        for index in range(19):
            name = f"pacpin-sequence-{'pure' if index < 10 else 'opt'}-{index:02}"
            directory = self.root / f"db/local/{name}-1-1"
            directory.mkdir(exist_ok=True)
            (directory / "desc").write_text(f"%NAME%\n{name}\n\n%VERSION%\n1-1\n\n%DESC%\nSelection handoff package {index}\n\n%REASON%\n1\n\n%SIZE%\n1\n\n%ARCH%\nx86_64\n\n")
            (directory / "files").write_text("%FILES%\n\n")
        keeper = self.root / "db/local/pacpin-sequence-keeper-1-1"
        keeper.mkdir(exist_ok=True)
        optional = "\n".join(f"pacpin-sequence-opt-{index:02}: optional integration" for index in range(10, 19))
        (keeper / "desc").write_text(f"%NAME%\npacpin-sequence-keeper\n\n%VERSION%\n1-1\n\n%REASON%\n0\n\n%SIZE%\n1\n\n%OPTDEPENDS%\n{optional}\n\n")
        (keeper / "files").write_text("%FILES%\n\n")

    def handoff(self):
        original = self.cfg.read_text()
        self.seed(source=False)
        self.publish(2)
        self.inventory()
        self.cfg.write_text(original.replace("smart_orphans = false", "smart_orphans = true"))
        SlowRepository.download_delay = .35
        self.case("direct-default-ten-selected-one-enter", ["-Su"], native=True,
                  confirm=True, orphan_choice=[], selection_wait=True, width=110, timeout=90)
        self.results[-1]["physical_selection_enters"] = 1
        remaining = set(run("pacman", "-Qq").splitlines())
        if any(f"pacpin-sequence-pure-{index:02}" in remaining for index in range(10)):
            self.results[-1]["failures"].append("confirmed pure orphans were not removed")
        if any(f"pacpin-sequence-opt-{index:02}" not in remaining for index in range(10,19)):
            self.results[-1]["failures"].append("unchecked optional packages were removed")
        self.checkpoint()

    def reset_case(self, name, native=True, source=False, orphans=False, external=(), delay=False):
        self.cfg.write_text(self.original_cfg)
        self.env["XDG_STATE_HOME"] = str(self.root / f"state-{name}")
        for directory in (self.root / "db/local").glob("pacpin-sequence-*"):
            shutil.rmtree(directory)
        subprocess.run(["sudo", "-n", "pacman", "-R", "--noconfirm", "hello"], capture_output=True)
        self.seed(native=native, source=source)
        self.publish(2 if native else 1)
        self.catalog(2 if source else 1)
        self.configure(names=external)
        config = self.original_cfg.replace("smart_orphans = false", f"smart_orphans = {str(orphans).lower()}")
        if delay:
            config = config.replace("[features]", "[features]\nstability_delays = true")
            config += '\n[delay]\npacpin-ui-native = 1000\n'
        self.cfg.write_text(config)
        if orphans:
            self.inventory()

    def matrix(self):
        self.original_cfg = self.cfg.read_text()
        entries = [
            ("native-default-enter", [], dict(orphans=True)),
            ("native-no-cache-enter", ["--no-cache"], dict(orphans=True)),
            ("native-storage-enter", ["--use-storage"], dict(orphans=True)),
            ("native-ram-enter", ["--use-ram"], dict(orphans=True)),
            ("native-keep-going-enter", ["--keep-going"], dict(orphans=True)),
            ("native-autoremove-enter", ["--autoremove"], dict(orphans=True)),
            ("native-no-orphans", [], {}),
            ("native-noconfirm-orphans", ["--noconfirm", "--autoremove"], dict(orphans=True)),
            ("native-refresh-enter", ["--refresh", "--no-cache"], dict(orphans=True)),
            ("native-sources-default-enter", [], dict(source=True, orphans=True)),
            ("native-sources-cache-storage", ["--no-cache", "--use-storage"], dict(source=True, orphans=True)),
            ("native-sources-cache-ram", ["--no-cache", "--use-ram"], dict(source=True, orphans=True)),
            ("sources-only", ["--sources-only"], dict(native=False, source=True, orphans=True)),
            ("sources-only-cache-storage", ["--sources-only", "--no-cache", "--use-storage"], dict(native=False, source=True)),
            ("external-only", ["--external-only"], dict(external=("flatpak", "nix"))),
            ("external-only-cache", ["--external-only", "--no-cache"], dict(external=("flatpak", "nix"))),
            ("native-external-enter", [], dict(orphans=True, external=("flatpak", "nix"))),
            ("native-source-external-cache", ["--no-cache"], dict(source=True, orphans=True, external=("flatpak", "nix"))),
            ("held-native-no-work", [], dict(delay=True)),
            ("held-native-source-enter", [], dict(source=True, orphans=True, delay=True)),
            ("held-native-external-enter", [], dict(orphans=True, delay=True, external=("flatpak", "nix"))),
            ("dry-native-orphans", ["--dry-run", "--no-cache"], dict(orphans=True)),
            ("dry-held-native", ["--dry-run"], dict(delay=True)),
        ]
        for name, flags, setup in entries:
            native = setup.get("native", True)
            source = setup.get("source", False)
            orphans = setup.get("orphans", False)
            external = setup.get("external", ())
            delayed = setup.get("delay", False)
            dry = "--dry-run" in flags
            external_only = "--external-only" in flags
            sources_only = "--sources-only" in flags
            effective_native = native and not delayed and not external_only and not sources_only
            effective_source = source and not external_only
            work = effective_native or effective_source or bool(external)
            self.reset_case(name, **setup)
            SlowRepository.download_delay = .35 if orphans and effective_native else .035
            review = orphans and not dry and not external_only and not sources_only
            approve = work and not dry and "--noconfirm" not in flags
            kwargs = dict(native=effective_native and not dry, source=effective_source and not dry,
                          confirm=approve, width=110, timeout=120, allow_ram_refusal="--use-ram" in flags)
            if review and "--noconfirm" not in flags:
                kwargs.update(orphan_choice=[])
            self.case(name, ["-Su", *flags], **kwargs)
            result = self.results[-1]
            result["selection_enters"] = 1 if kwargs.get("orphan_choice") == [] else 0
            result["external_adapters"] = "controlled literal-command fixtures" if external else "none"
            if result["status"] == 0 and not dry:
                remaining = set(run("pacman", "-Qq").splitlines())
                remove = review and ("--noconfirm" not in flags or "--autoremove" in flags)
                if orphans and any((f"pacpin-sequence-pure-{i:02}" in remaining) == remove for i in range(10)):
                    result["failures"].append("orphan removal did not match approved scope")
                if orphans and any(f"pacpin-sequence-opt-{i:02}" not in remaining for i in range(10,19)):
                    result["failures"].append("unchecked optional package removed")
            if (self.root / "db/db.lck").exists():
                result["failures"].append("ALPM lock retained")
            if external:
                frames = json.loads((self.root / f"{name}.frames.json").read_text())
                text = transcript(self.root / f"{name}.raw.txt")
                for label in ("Flatpak", "Nix"):
                    if not any(re.search(rf"(?:^|\n){label}: (?!Pending|✔)", frame) for frame in frames):
                        result["failures"].append(f"{label} active footer stage missing")
                    if not dry and f"{label} upgrade completed successfully" not in text:
                        result["failures"].append(f"{label} verified completion missing")
                if not dry and not any("Flatpak: ✔ Upgrade complete" in frame and re.search(r"(?:^|\n)Nix: (?!Pending|✔)", frame) for frame in frames):
                    result["failures"].append("completed Flatpak row missing during Nix work")
            self.checkpoint()

    def timing(self):
        self.original_cfg = self.cfg.read_text()
        entries = [
            ("download-active-one-enter", .3, 110, [], False, False),
            ("download-finished-before-enter", 9, 110, [], False, False),
            ("review-crosses-sudo-renewal", 65, 110, [], False, False),
            ("narrow-default-selection", .3, 40, [], False, False),
            ("search-one-enter", .3, 110, "/pure-09", False, False),
            ("none-selected-one-enter", .3, 110, ["n"], False, False),
            ("download-failure-after-selection", .3, 110, [], True, False),
            ("interrupt-download-wait", .3, 110, [], False, True),
        ]
        for name, delay, width, choice, failed, cancel in entries:
            self.reset_case(name, source=True, orphans=True)
            SlowRepository.download_delay = .35
            self.orphan_enter_delay = delay
            if failed:
                for archive in self.repo.glob("*.pkg.tar.zst"):
                    archive.unlink()
            self.case(name, ["-Su", "--no-cache"], native=True, source=True,
                      confirm=True, orphan_choice=choice, width=width, timeout=150,
                      selection_wait=not failed and delay < 1,
                      selection_wait_cancel=cancel, success=not (failed or cancel),
                      expected_error="failed retrieving file" if failed else None)
            result = self.results[-1]
            result["selection_enters"] = 1
            result["review_seconds"] = delay
            remaining = set(run("pacman", "-Qq").splitlines())
            keep = failed or cancel or choice == ["n"]
            if any((f"pacpin-sequence-pure-{i:02}" in remaining) != keep for i in range(10)):
                result["failures"].append("orphan removal differs from selection/transaction outcome")
            if (self.root / "db/db.lck").exists():
                result["failures"].append("ALPM lock retained after handoff/interruption")
            self.checkpoint()

    def authentication(self, fresh_only=False):
        self.original_cfg = self.cfg.read_text()
        policy = Path("/etc/sudoers.d/zz-pacpin-ui-auth")
        if policy.exists():
            raise AssertionError("Refusing to overwrite a guest sudoers policy")
        fixture = self.root / "sudoers-auth"
        fixture.write_text("Defaults:garuda timestamp_type=tty,timestamp_timeout=1.5,verifypw=all\ngaruda ALL=(root) PASSWD: /usr/bin/pacman\n")
        run("/usr/bin/sudo", "-n", "visudo", "-cf", str(fixture))
        entries = [("password-prefetch-install-source-cache", .3, False),
                   ("password-next-invocation-cached-ticket", .3, True),
                   ("password-long-orphan-review", 100, False)]
        for name, delay, cached in entries[:2] if fresh_only else entries:
            self.reset_case(name, source=True, orphans=True)
            self.orphan_enter_delay = delay
            SlowRepository.download_delay = .35
            run("/usr/bin/sudo", "-n", "install", "-m", "0440", str(fixture), str(policy))
            try:
                helper = str(self.sudo_dir / "sudo") if self.sudo_dir else "/usr/bin/sudo"
                probe = subprocess.run([helper, "-n", "pacman", "--version"], capture_output=True)
                if probe.returncode == 0:
                    raise AssertionError("Password policy did not apply")
                if cached:
                    marker = self.root / "ticket-before-next-invocation.status"
                    self.rc("send-text", "--stdin", input=f"{shlex.quote(helper)} -n -v; printf '%s' \"$?\" > {shlex.quote(str(marker))}\n")
                    deadline = time.monotonic() + 5
                    while not marker.exists() and time.monotonic() < deadline:
                        time.sleep(.05)
                    if not marker.exists() or marker.read_text() != "0":
                        raise AssertionError("No valid cached terminal ticket before the next invocation")
                # Reseed between invocations: a no-work second run correctly
                # needs no authentication and cannot test fresh credentials.
                self.case(name, ["-Su", "--no-cache"], native=True, source=True,
                          confirm=True, orphan_choice=[], password=True,
                          password_prompt=self.password_prompt,
                          timeout=180, width=110)
                self.results[-1]["cached_ticket_before_run"] = cached
                if "Native cache cleaned successfully" not in transcript(self.root / f"{name}.raw.txt"):
                    self.results[-1]["failures"].append("successful native cleanup was not reported")
                self.checkpoint()
            finally:
                run("/usr/bin/sudo", "-n", "rm", "--", str(policy))

    def aur_sequences(self):
        self.original_cfg = self.cfg.read_text()
        build = self.root / "aur-old"
        build.mkdir()
        (build / "PKGBUILD").write_text("pkgname=hello\npkgver=0.0.0\npkgrel=1\narch=('x86_64')\npackage() { install -Dm644 /etc/os-release \"$pkgdir/opt/pacpin-ui-aur/placeholder\"; }\n")
        run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=build)
        old = next(build.glob("*.pkg.tar.zst"))
        paru_dir = self.root / "config/paru"
        paru_dir.mkdir()
        (paru_dir / "paru.conf").write_text("[options]\nSkipReview\n")
        makepkg_config = self.root / "aur-makepkg.conf"
        makepkg_config.write_text(Path("/etc/makepkg.conf").read_text() + "\nOPTIONS=(strip docs !libtool !staticlibs emptydirs zipman purge !debug !lto)\n")
        self.env["MAKEPKG_CONF"] = str(makepkg_config)
        for name, native, source, flags in [
            ("aur-only-default-orphans", False, False, ["--aur-only"]),
            ("native-aur-default-orphans", True, False, []),
            ("aur-source-default-orphans", False, True, []),
            ("native-aur-source-storage-cache", True, True, ["--use-storage", "--no-cache"]),
        ]:
            self.reset_case(name, native=native, source=source, orphans=True)
            self.cfg.write_text(self.cfg.read_text().replace('[pins]', '[pins]\nhello = "aur"'))
            run("/usr/bin/sudo", "-n", "pacman", "-U", "--noconfirm", str(old))
            scoped = "--aur-only" in flags
            SlowRepository.download_delay = .35
            self.case(name, ["-Su", *flags], native=native, source=source, aur=True,
                      confirm=True, orphan_choice=None if scoped else [], width=110, timeout=300)
            self.results[-1]["aur_runner"] = "real paru, makepkg and AUR hello recipe"
            frames = json.loads((self.root / f"{name}.frames.json").read_text())
            if native and not any("Pacman: Downloading" in frame and "AUR: Pending" in frame for frame in frames):
                self.results[-1]["failures"].append("AUR pending row missing during native downloads")
            if source and not any("AUR: ✔ Upgrade complete" in frame and re.search(r"(?:^|\n)Sources:", frame) for frame in frames):
                self.results[-1]["failures"].append("completed AUR row missing during Sources")
            remaining = set(run("pacman", "-Qq").splitlines())
            if any((f"pacpin-sequence-pure-{i:02}" in remaining) != scoped for i in range(10)):
                self.results[-1]["failures"].append("orphan cleanup differs from AUR scope")
            if (self.root / "db/db.lck").exists():
                self.results[-1]["failures"].append("ALPM lock retained after AUR sequence")
            self.checkpoint()

    def suite(self):
        if self.section == "matrix":
            self.matrix()
        elif self.section == "timing":
            self.timing()
        elif self.section == "auth":
            self.authentication()
        elif self.section == "auth-fresh":
            self.authentication(fresh_only=True)
        elif self.section == "aur":
            self.aur_sequences()
        else:
            self.handoff()


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--section", choices=["handoff", "matrix", "timing", "auth", "auth-fresh", "aur"], default="handoff")
    parser.add_argument("--sudo-dir", type=Path)
    parser.add_argument("--password-prompt", help="Override the guest's actual sudo/PAM prompt for authentication checks")
    parser.add_argument("--job-control", action="store_true", help="Launch pacpin in its own foreground process group, as an interactive shell does")
    parser.add_argument("--orphan-enter-delay", type=float, default=.3)
    args = parser.parse_args()
    if (os.environ.get("PACPIN_DISPOSABLE_VM") != "1"
            or "ID=garuda" not in Path("/etc/os-release").read_text()
            or run("systemd-detect-virt").strip() != "oracle"
            or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay"
            or os.geteuid() == 0):
        raise SystemExit("Requires regular user in disposable Garuda live VM")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        raise SystemExit("Use a fresh output directory")
    checks = None
    try:
        checks = SequenceChecks(args.binary, args.output, "unix:/tmp/pacpin-ui-kitty.sock", False, False, args.section)
        checks.job_control = args.job_control
        checks.orphan_enter_delay = args.orphan_enter_delay
        checks.sudo_dir = args.sudo_dir
        helper = (args.sudo_dir or Path("/usr/bin")) / "sudo"
        version = run(str(helper), "--version").splitlines()[0]
        checks.password_prompt = args.password_prompt or (
            "[sudo: authenticate] Password:" if version.startswith("sudo-rs ") else "PACPIN_AUTH_PASSWORD:"
        )
        (args.output / "privilege-helper.json").write_text(json.dumps({
            "path": str(helper), "version": version,
            "job_control": args.job_control,
        }, indent=2))
        if args.sudo_dir:
            checks.env["PATH"] = str(args.sudo_dir) + ":" + checks.env["PATH"]
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(item["failures"] for item in checks.results):
        raise SystemExit("Sequence checks failed")


if __name__ == "__main__":
    main()
