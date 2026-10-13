#!/usr/bin/env python3
"""Run ONLY in the disposable Garuda VM, with its Kitty test socket open.

Uses real makepkg, pacman transactions and rendered Kitty screen/scrollback.
The guest pacman config is restored on exit. Never run this on the host.
"""
import argparse
import functools
import fcntl
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import threading
import time


def run(*args, **kwargs):
    result = subprocess.run(args, text=True, capture_output=True, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{shlex.join(map(str, args))}:\n{result.stdout}\n{result.stderr}")
    return result.stdout


def transcript(path):
    # script's header includes the command, including SUDO_PROMPT. It is
    # metadata rather than terminal output and must not count as a prompt.
    return "\n".join(line for line in path.read_text(errors="replace").splitlines()
                     if not line.startswith(("Script started on ", "Script done on ")))


def serialized_verifier(main):
    """All suites temporarily own guest pacman.conf and the same terminal."""
    @functools.wraps(main)
    def wrapped():
        with open("/tmp/pacpin-vm-verifier.lock", "a") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise SystemExit("Another pacpin VM verifier is running; run suites sequentially")
            return main()
    return wrapped


class SlowRepository(http.server.SimpleHTTPRequestHandler):
    download_delay = 0.035

    def log_message(self, *_):
        pass

    def copyfile(self, source, output):
        while chunk := source.read(64 * 1024):
            output.write(chunk)
            if self.path.endswith((".pkg.tar.zst", "source.tar")):
                time.sleep(self.download_delay)


class Checks:
    def __init__(self, binary, root, socket, baseline, aur, section):
        self.binary, self.root, self.socket, self.baseline = binary, root, socket, baseline
        self.test_aur = aur
        self.section = section
        self.results = []
        self.env = dict(os.environ, XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"))
        self.repo = root / "repo"
        self.repo.mkdir()
        self.artifact = root / "source.tar"
        payload = root / "payload"
        (payload / "bin").mkdir(parents=True)
        shutil.copyfile("/usr/bin/true", payload / "bin/app")
        (payload / "padding").write_bytes(os.urandom(8 * 1024 * 1024))
        with tarfile.open(self.artifact, "w") as archive:
            archive.add(payload / "bin", arcname="bin")
            archive.add(payload / "padding", arcname="padding")
        self.sha = hashlib.sha256(self.artifact.read_bytes()).hexdigest()
        # Slow only reads of this local fixture so a real download can be
        # inspected/resized/interrupted without allocating a giant archive.
        # No production code or networking/TLS policy is modified for tests.
        slow_read = root / "slow-read.c"
        slow_read.write_text(r'''
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static ssize_t (*original_read)(int, void *, size_t);
__attribute__((constructor)) static void init(void) {
    original_read = dlsym(RTLD_NEXT, "read");
}
ssize_t read(int fd, void *buffer, size_t size) {
    char descriptor[64], target[4096];
    const char *fixture = getenv("PACPIN_UI_SLOW_ARTIFACT");
    snprintf(descriptor, sizeof descriptor, "/proc/self/fd/%d", fd);
    ssize_t length = readlink(descriptor, target, sizeof target - 1);
    if (fixture && length >= 0) {
        target[length] = 0;
        if (!strcmp(target, fixture)) usleep(35000);
    }
    return original_read(fd, buffer, size);
}
''')
        preload = root / "slow-read.so"
        run("cc", "-shared", "-fPIC", "-o", str(preload), str(slow_read), "-ldl")
        self.env.update(LD_PRELOAD=str(preload), PACPIN_UI_SLOW_ARTIFACT=str(self.artifact))
        (root / "config/pacpin").mkdir(parents=True)
        self.cfg = root / "config/pacpin/config.toml"
        self.cfg.write_text(f'''repo_order = ["pacpin-ui"]
[features]
shell_aliases = false
smart_orphans = false
integrations = false
[options]
privilege_tool = "sudo"
helper = "paru"
[pins]
pacpin-ui-native = "pacpin-ui"
pacpin-ui-source = "ui-local"
[sources.ui-local]
kind = "local"
url = "{root}/catalog.json"
[sources.ui-local.packages.pacpin-ui-source]
asset = "source.tar"
format = "archive"
depends = []
binaries = {{ pacpin-ui-source = "bin/app" }}
''')
        os.chmod(self.cfg, 0o600)
        handler = functools.partial(SlowRepository, directory=str(self.repo))
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        for directory in ["db/local", "db/sync", "cache", "hooks"]:
            (root / directory).mkdir(parents=True)
        # Live-ISO snapshot hooks depend on the original installed database.
        # Shadow them only in this isolated database's custom hook directory.
        for hook in Path("/usr/share/libalpm/hooks").glob("*.hook"):
            (root / "hooks" / hook.name).symlink_to("/dev/null")
        hook_script = root / "fixture-hook.sh"
        hook_script.write_text("#!/bin/bash\nfor i in $(seq 1 5); do printf 'PACPIN_NATIVE_HOOK_%s\\n' \"$i\"; sleep 0.15; done\n")
        os.chmod(hook_script, 0o755)
        (root / "hooks/zz-pacpin-ui.hook").write_text(f"[Trigger]\nOperation = Upgrade\nType = Package\nTarget = pacpin-ui-native\n[Action]\nDescription = pacpin UI test hook\nWhen = PostTransaction\nExec = {hook_script}\n")
        (root / "db/local/ALPM_DB_VERSION").write_text("9\n")
        config = root / "pacman.conf"
        config.write_text(f'''[options]
Architecture = auto
DBPath = {root}/db
CacheDir = {root}/cache
HookDir = {root}/hooks
SigLevel = Never
LocalFileSigLevel = Never
CheckSpace
Color
[pacpin-ui]
Server = http://127.0.0.1:{self.server.server_port}
''')
        self.original = Path("/etc/pacman.conf").read_bytes()
        (root / "pacman.original.conf").write_bytes(self.original)
        run("sudo", "-n", "cp", str(config), "/etc/pacman.conf")
        # A preceding baseline run used a different isolated database. Remove
        # only this verifier's four known guest paths before creating fixtures.
        run("sudo", "-n", "rm", "-rf", "--", "/opt/pacpin-ui-native", "/opt/pacpin-ui-source", "/usr/lib/pacpin-ui-test", "/usr/bin/pacpin-ui-source")
        self.native = {}
        for version in [1, 2]:
            build = root / f"native-{version}"
            build.mkdir()
            (build / "padding").write_bytes(os.urandom(1024 * 1024))
            (build / "fixture.install").write_text('''post_upgrade() {
  for i in $(seq -w 1 40); do
    printf 'PACPIN_NATIVE_HISTORY_%s\\n' "$i"
    sleep 0.02
  done
  sleep 1
}
''')
            (build / "PKGBUILD").write_text(f'''pkgname=pacpin-ui-native
pkgver={version}.0.0
pkgrel=1
arch=('x86_64')
options=('!strip' '!debug')
install=fixture.install
package() {{
  install -Dm644 "$startdir/padding" "$pkgdir/opt/pacpin-ui-native/padding"
  install -Dm755 /usr/lib/libz.so.1 "$pkgdir/usr/lib/pacpin-ui-test/libui.so"
}}
''')
            run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=build)
            self.native[version] = next(build.glob("*.pkg.tar.zst"))

    def rc(self, *args, **kwargs):
        return run("kitty", "@", "--to", self.socket, *args, **kwargs)

    def catalog(self, version, valid=True):
        (self.root / "catalog.json").write_text(json.dumps({"version": 1, "packages": {"pacpin-ui-source": [{"tag": f"v{version}.0.0", "assets": [{"name": "source.tar", "url": "source.tar", "sha256": self.sha if valid else "0" * 64}]}]}}))

    def seed(self, source=True, native=True):
        self.catalog(1)
        if native:
            run("sudo", "-n", "pacman", "-U", "--noconfirm", str(self.native[1]))
        else:
            subprocess.run(["sudo", "-n", "pacman", "-R", "--noconfirm", "pacpin-ui-native"], capture_output=True)
        if source:
            run(str(self.binary), "-S", "ui-local/pacpin-ui-source", "--noconfirm", env=self.env)
        else:
            subprocess.run(["sudo", "-n", "pacman", "-R", "--noconfirm", "pacpin-ui-source"], capture_output=True)

    def publish(self, version):
        for path in self.repo.iterdir():
            if path.name == "source.tar":
                continue
            path.unlink()
        shutil.copyfile(self.native[version], self.repo / self.native[version].name)
        run("repo-add", str(self.repo / "pacpin-ui.db.tar.gz"), str(self.repo / self.native[version].name))
        for path in (self.root / "cache").iterdir():
            if path.is_file():
                path.unlink()
        run("sudo", "-n", "pacman", "-Syy", "--noconfirm")

    def case(self, name, args, native=False, source=False, success=True, resize=False, confirm=False, width=80, cancel=False, expected_error=None, aur=False, password=False, repeat=1, timeout=None, selection=False, selection_cancel=False, repo_menu=False, selection_wait=False, selection_delay=0.6, selection_wait_cancel=False, source_completed=False, allow_ram_refusal=False, aur_package="hello", orphan_choice=None, expect_no_orphan_prompt=False, orphans=None, cache=None, cancel_manager=None, completed_before_cache=None, cancel_marker=None, password_prompt="PACPIN_AUTH_PASSWORD:", resize_password=False):
        result = self.root / f"{name}.status"
        result.unlink(missing_ok=True)
        raw = self.root / f"{name}.raw.txt"
        # Deliberately stale: live terminal dimensions must beat COLUMNS.
        env_keys = ["HOME", "PATH", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "LD_PRELOAD", "PACPIN_UI_SLOW_ARTIFACT"] + (["TMPDIR"] if "TMPDIR" in self.env else [])
        command = shlex.join(["env", *[f"{key}={self.env[key]}" for key in env_keys], "COLUMNS=120", str(self.binary), *args])
        if password:
            # Consecutive runs share script's controlling tty and sudo ticket.
            # pacpin itself must require fresh authentication for the next run.
            command = "export SUDO_PROMPT=" + shlex.quote(password_prompt + " ") + "; " + " && ".join([command] * repeat)
        scratch_before = {str(path) for base in ["/tmp", "/var/tmp"] for path in Path(base).glob("pacpin-source-*")}
        shell = self.root / f"{name}.sh"
        shell.write_text(f"#!/bin/bash\nprintf '\\033[2J\\033[3J\\033[H'\nprintf 'PACPIN_BEFORE_TRANSACTION\\n'\nstty -g > {shlex.quote(str(self.root / (name + '.term-before')))}\nscript -qef -c {shlex.quote(command)} {shlex.quote(str(raw))}\nstatus=$?\nstty -g > {shlex.quote(str(self.root / (name + '.term-after')))}\nprintf 'PACPIN_AFTER_TRANSACTION\\n'\nprintf '%s' \"$status\" > {shlex.quote(str(result))}\n")
        self.rc("resize-os-window", "--width", str(width), "--height", "30", "--unit", "cells")
        resize_deadline = time.monotonic() + 3
        while True:
            window = json.loads(self.rc("ls"))[0]["tabs"][0]["windows"][0]
            if window["columns"] == width and window["lines"] == 30:
                break
            if time.monotonic() > resize_deadline:
                raise AssertionError(f"Kitty resize did not settle: {window['columns']}x{window['lines']}, requested {width}x30")
            time.sleep(.05)
        self.rc("send-text", "--stdin", input=f"bash {shlex.quote(str(shell))}\n")
        snapshots, resized, confirmed, cancelled = [], False, False, False
        answered_passwords = 0
        pending_password_since = None
        password_visibility_failures = []
        selection_started, selection_enter = None, False
        deadline = time.monotonic() + (timeout or (180 if aur else 60))
        while not result.exists():
            if time.monotonic() > deadline:
                self.rc("send-key", "ctrl+c")
                raise AssertionError(f"{name} timed out")
            screen = self.rc("get-text", "--extent", "screen")
            snapshots.append(screen)
            prompts = transcript(raw).count(password_prompt) if password and raw.exists() else 0
            if password and prompts > answered_passwords and prompts <= repeat:
                if pending_password_since is None:
                    pending_password_since = time.monotonic()
                if resize_password and not resized and time.monotonic() - pending_password_since > .5:
                    self.rc("resize-os-window", "--width", "40", "--height", "18", "--unit", "cells")
                    resized = True
                if time.monotonic() - pending_password_since < 2:
                    time.sleep(.08)
                    continue
                if password_prompt not in screen:
                    password_visibility_failures.append(f"password prompt {prompts} not visible before input")
                (self.root / f"{name}-password-{prompts}.screen.txt").write_text(screen)
                # Disposable live account only. Never request/store a host password.
                self.rc("send-text", "--stdin", input="garuda\n")
                answered_passwords += 1
                pending_password_since = None
            if password and prompts > repeat:
                (self.root / f"{name}.screen.txt").write_text(self.rc("get-text", "--extent", "all"))
                (self.root / f"{name}.frames.json").write_text(json.dumps(snapshots))
                self.results.append({"case": name, "status": None, "failures": ["sudo requested authentication again in the same transaction"], "password_prompts": transcript(raw).count(password_prompt)})
                self.rc("send-key", "ctrl+c")
                raise AssertionError(f"{name}: sudo requested authentication again in the same transaction")
            if confirm and not confirmed and "Proceed with installation?" in screen:
                self.rc("send-text", "--stdin", input="y\n")
                confirmed = True
            if orphan_choice is not None and selection_started is None and "Showing " in screen and "Enter confirms" in screen:
                if isinstance(orphan_choice, str):
                    self.rc("send-text", "--stdin", input=orphan_choice)
                else:
                    for key in orphan_choice:
                        self.rc("send-key", key)
                selection_started = time.monotonic()
            if orphan_choice is not None and selection_started and not selection_enter and time.monotonic() - selection_started > 0.3:
                self.rc("send-key", "enter")
                selection_enter = True
            if selection and selection_started is None and "Showing " in screen and "Enter confirms" in screen:
                self.rc("send-text", "--stdin", input="n")
                self.rc("send-key", "end")
                selection_started = time.monotonic()
            if selection_cancel and not cancelled and "Showing " in screen and "Enter confirms" in screen:
                self.rc("send-key", "ctrl+c")
                cancelled = True
            if repo_menu and not cancelled and "Repository priority" in screen:
                self.rc("send-text", "--stdin", input="q")
                cancelled = True
            if selection_started and not selection_enter and time.monotonic() - selection_started > selection_delay:
                # A single Enter while search is active must confirm too.
                self.rc("send-text", "--stdin", input="/pacpin-ui-orphan")
                self.rc("send-key", "enter")
                selection_enter = True
            if selection_wait_cancel and not cancelled and "Waiting for repository downloads to finish..." in " ".join(screen.split()):
                self.rc("send-key", "ctrl+c")
                cancelled = True
            if resize and not resized and re.search(r"(?:Pacman|Sources):", screen):
                self.rc("resize-os-window", "--width", "40", "--height", "18", "--unit", "cells")
                resized = True
            if cancel and not cancelled and "Sources:" in screen and "MiB" in screen:
                self.rc("send-key", "ctrl+c")
                cancelled = True
            if cancel_manager and not cancelled and re.search(rf"(?:^|\n){cancel_manager}: (?!✔)", screen) and (cancel_marker is None or cancel_marker in screen):
                if cancel_marker and not (self.root / "db/db.lck").exists():
                    raise AssertionError("Cancellation fixture did not hold the ALPM database lock")
                self.rc("send-key", "ctrl+c")
                cancelled = True
            time.sleep(0.08)
        # The status file is independent of Kitty's display event loop.
        # Wait for its trailing output to render before judging scrollback.
        render_deadline = time.monotonic() + 2
        while True:
            all_text = self.rc("get-text", "--extent", "all")
            if "PACPIN_AFTER_TRANSACTION" in all_text or time.monotonic() >= render_deadline:
                break
            time.sleep(.05)
        (self.root / f"{name}.screen.txt").write_text(all_text)
        (self.root / f"{name}.frames.json").write_text(json.dumps(snapshots))
        status = int(result.read_text())
        if allow_ram_refusal and status != 0 and "Insufficient scratch capacity for Ram policy" in transcript(raw):
            # MemAvailable can change while the command plans/extracts. Accept
            # only the explicit RAM guard, never an arbitrary build failure.
            success = False
        failures = []
        failures.extend(password_visibility_failures)
        if (status == 0) != success:
            failures.append(f"exit status {status}")
        if "PACPIN_BEFORE_TRANSACTION" not in all_text or "PACPIN_AFTER_TRANSACTION" not in all_text:
            failures.append("prior/following terminal text overwritten")
        if native and success:
            missing = [f"PACPIN_NATIVE_HISTORY_{i:02}" for i in range(1, 41) if f"PACPIN_NATIVE_HISTORY_{i:02}" not in all_text]
            if missing:
                failures.append(f"pacman history overwritten: {missing}")
            if any("PACPIN_NATIVE_HISTORY_" in line and not line.startswith("PACPIN_NATIVE_HISTORY_") for line in all_text.splitlines()):
                failures.append("native output indented by missing carriage returns")
        if any("Native/AUR:" in frame for frame in snapshots) or (not aur and any(re.search(r"(?:^|\n)AUR:", frame) for frame in snapshots)):
            failures.append("AUR shown although it was never invoked")
        if aur and not any(re.search(r"(?:^|\n)AUR:", frame) for frame in snapshots):
            failures.append("invoked AUR helper progress missing")
        if not native and any(re.search(r"(?:^|\n)Pacman:", frame) for frame in snapshots):
            failures.append("Native shown although it was never invoked")
        if any("Finished; waiting" in frame for frame in snapshots):
            failures.append("blanket manager completion")
        for manager, expected in [("Orphans", orphans), ("Cache", cache)]:
            present = any(re.search(rf"(?:^|\n){manager}: (?!✔)", frame) for frame in snapshots)
            if expected is not None and present != expected:
                failures.append(f"{manager} footer scope mismatch: expected {expected}, observed {present}")
        if cancel_manager and not cancelled:
            failures.append(f"Ctrl-C during {cancel_manager} was not exercised")
        if completed_before_cache and not any(f"{completed_before_cache}: ✔" in frame and re.search(r"(?:^|\n)Cache: (?!✔)", frame) for frame in snapshots):
            failures.append(f"completed {completed_before_cache} row absent during cleanup")
        if source and (success or source_completed) and not native and not any(re.search(r"(?:^|\n)Sources:", frame) for frame in snapshots):
            failures.append("source-only progress missing")
        if not source and any(re.search(r"(?:^|\n)Sources:", frame) for frame in snapshots):
            failures.append("Sources shown although it was never invoked")
        before, after = [self.root / f"{name}.term-{part}" for part in ["before", "after"]]
        if before.read_text() != after.read_text():
            failures.append("terminal settings not restored")
        if confirm and not confirmed:
            failures.append("confirmation prompt not exercised")
        if selection and (not selection_enter or not any("Pacman: Download" in frame and "Showing " in frame for frame in snapshots)):
            failures.append("single Enter or background repository download footer not exercised")
        if native and source and success and not any("Pacman: ✔ Upgrade complete" in frame and "Sources:" in frame for frame in snapshots):
            failures.append("completed Pacman row absent while Sources runs")
        if resize and not resized:
            failures.append("live footer resize not exercised")
        if cancel and not cancelled:
            failures.append("Ctrl-C during download not exercised")
        if (selection_cancel or selection_wait_cancel or repo_menu) and not cancelled:
            failures.append("menu cancellation not exercised")
        if selection and not selection_enter:
            failures.append("single Enter confirmation not exercised")
        if orphan_choice is not None and not selection_enter:
            failures.append("pending orphan cleanup was not offered for fresh confirmation")
        if expect_no_orphan_prompt and any("Enter confirms" in frame for frame in snapshots):
            failures.append("orphan cleanup unexpectedly prompted after explicit decline/completion")
        if selection_wait and not any("Waiting for repository downloads to finish..." in " ".join(frame.split()) and re.search(r"(?:^|\n)Pacman:.*(?:Downloading|Verifying)", frame) for frame in snapshots):
            failures.append("repository download footer missing after Enter while downloads remain active")
        raw_text = transcript(raw)
        if native and success and "PACPIN_NATIVE_HOOK_1" in raw_text and not any("Pacman: Running post-install hooks" in frame for frame in snapshots):
            failures.append("native hook output did not update the footer phase")
        plain = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", raw_text)
        # Initial plan boxes must fit even though COLUMNS is deliberately 120.
        if any(len(line) > width for line in plain.splitlines() if line.startswith("  ┌") or line.startswith("  └")):
            failures.append("plan border exceeds the measured terminal width")
        if password and (answered_passwords != repeat or raw_text.count(password_prompt) != repeat):
            failures.append(f"expected exactly one sudo password prompt per run ({repeat} runs)")
        if password and re.search(re.escape(password_prompt) + r" *garuda", raw_text):
            failures.append("sudo password echoed into the transcript")
        if source and args[0] == "-Su" and not self.baseline and "Unknown" not in raw_text and "+ unknown" not in raw_text:
            failures.append("unknown source sizes shown as a complete total")
        final_screen = self.rc("get-text", "--extent", "screen")
        if re.search(r"(?:^|\n)(?:Pacman|AUR|Sources|Flatpak|Nix|Pipx|Hooks|Orphans|Cache):", final_screen):
            failures.append("active footer left behind after the command ended")
        if expected_error and expected_error not in raw_text:
            failures.append(f"expected error missing: {expected_error}")
        if not success and not source_completed and "Sources upgrade completed successfully" in raw_text:
            failures.append("source failure reported as success")
        if selection and "Orphan cleanup skipped (no packages selected)." not in " ".join(plain.split()):
            failures.append("orphan confirmation did not print the empty selection result")
        if not success and ":: Sources: Waiting for repository downloads to finish" in raw_text:
            failures.append("queued source work still reported as waiting after native failure")
        scratch_after = {str(path) for base in ["/tmp", "/var/tmp"] for path in Path(base).glob("pacpin-source-*")}
        if scratch_after - scratch_before:
            failures.append(f"source scratch leaked: {sorted(scratch_after - scratch_before)}")
        query = subprocess.run(["pacman", "-Q"], capture_output=True, text=True)
        if query.returncode not in [0, 1]:
            raise RuntimeError(query.stderr)
        versions = query.stdout.strip().splitlines()
        if (success or source_completed) and source and not any(line == "pacpin-ui-source 2.0.0-1" for line in versions):
            failures.append("source installed version is not 2.0.0-1")
        if success and native and not any(line == "pacpin-ui-native 2.0.0-1" for line in versions):
            failures.append("native installed version is not 2.0.0-1")
        if aur and success and not any(line.startswith(f"{aur_package} 2.") for line in versions):
            failures.append(f"AUR fixture {aur_package} was not installed")
        self.results.append({"case": name, "status": status, "failures": failures, "frames": len(snapshots), "resized": resized, "cancelled": cancelled, "installed": versions})
        if password:
            self.results[-1]["password_prompts"] = raw_text.count(password_prompt)
            self.results[-1]["runs"] = repeat
        print(json.dumps(self.results[-1]), flush=True)

    def suite(self):
        if self.section == "progress":
            self.progress_cases()
            return
        if self.section == "audit":
            helpers = self.root / "helpers"
            helpers.mkdir()
            self.advisory()
            self.audit_cases(helpers)
            self.auxiliary_failure_cases(helpers)
            return
        if self.section == "auth":
            self.auth_cases()
            if self.test_aur:
                self.aur_cases(password=True)
            return
        if self.section == "remaining":
            self.seed()
            self.publish(2)
            self.catalog(2)
            self.case("final-native-and-sources", ["-Su", "--noconfirm"], native=True, source=True)
            self.seed()
            self.publish(2)
            self.catalog(2)
            for path in self.repo.glob("*.pkg.tar.zst"):
                path.unlink()
            self.case("final-native-download-failure", ["-Su", "--noconfirm"], native=True, source=True, success=False, expected_error="failed retrieving file")
            self.seed()
            self.publish(1)
            self.catalog(2)
            self.case("final-sources-narrow", ["-Su", "--noconfirm"], source=True, width=32)
            frames = json.loads((self.root / "final-sources-narrow.frames.json").read_text())
            assert any("MiB" in frame and "%" in frame and "Sources:" in frame for frame in frames)
            helpers = self.root / "helpers"
            helpers.mkdir()
            self.remaining_suite(helpers)
            return
        self.seed()
        self.publish(1)
        self.catalog(2)
        self.case("sources-only", ["-Su", "--noconfirm"], source=True)
        self.seed()
        self.publish(2)
        self.catalog(2)
        self.case("native-and-sources", ["-Su", "--noconfirm"], native=True, source=True)
        if self.baseline:
            return
        self.seed(source=False)
        self.publish(2)
        self.case("native-only", ["-Su", "--noconfirm"], native=True)
        self.seed(source=False)
        self.publish(2)
        self.case("native-install", ["-S", "pacpin-ui/pacpin-ui-native", "--noconfirm"], native=True)
        self.seed()
        self.publish(2)
        self.catalog(2)
        self.case("mixed-resize-confirm", ["-Su"], native=True, source=True, resize=True, confirm=True)
        self.seed()
        self.publish(1)
        self.catalog(2)
        self.case("sources-resize", ["-Su", "--noconfirm"], source=True, resize=True)
        self.seed()
        self.publish(2)
        self.catalog(2)
        self.case("mixed-narrow", ["-Su", "--noconfirm"], native=True, source=True, width=40)
        self.seed()
        self.publish(1)
        self.catalog(2, valid=False)
        self.case("source-checksum-failure", ["-Su", "--noconfirm"], source=True, success=False, expected_error="SHA-256 mismatch")
        assert run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1")
        self.seed()
        self.publish(1)
        self.catalog(2)
        self.case("source-interrupted", ["-Su", "--noconfirm"], source=True, success=False, cancel=True)
        assert run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1")
        self.seed()
        self.publish(2)
        self.catalog(2)
        for path in self.repo.glob("*.pkg.tar.zst"):
            path.unlink()
        self.case("native-download-failure", ["-Su", "--noconfirm"], native=True, source=True, success=False, expected_error="failed retrieving file")
        assert run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1")
        self.seed()
        self.publish(1)
        self.case("no-updates", ["-Su", "--noconfirm"])
        self.seed()
        self.publish(1)
        self.catalog(2)
        helpers = self.root / "helpers"
        helpers.mkdir()
        makepkg = helpers / "makepkg"
        makepkg.write_text('#!/bin/sh\nif [ "$1" = "--version" ]; then exec /usr/bin/makepkg "$@"; fi\necho "PACPIN_TEST_PACKAGING_FAILURE" >&2\nexit 23\n')
        makepkg.chmod(0o755)
        original_path = self.env["PATH"]
        self.env["PATH"] = f"{helpers}:{original_path}"
        try:
            self.case("source-packaging-failure", ["-Su", "--noconfirm"], source=True, success=False, expected_error="Local source packaging failed")
            assert run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1")
        finally:
            self.env["PATH"] = original_path
            makepkg.unlink()
        self.integration_cases(helpers)
        self.remaining_suite(helpers)
        self.audit_cases(helpers)
        self.auxiliary_failure_cases(helpers)

    def progress_cases(self):
        # Unequal real archives make individual and aggregate percentages
        # visibly different. Both foreground and orphan-review downloads use
        # real pacman, not a simulated progress producer.
        large = self.root / "progress-large"
        large.mkdir()
        (large / "padding").write_bytes(os.urandom(3 * 1024 * 1024))
        (large / "fixture.install").write_text("post_install() { echo PACPIN_PROGRESS_FIRST_PACKAGE_COMPLETED; sleep 1; }\n")
        (large / "PKGBUILD").write_text('''pkgname=pacpin-ui-progress-large
pkgver=1
pkgrel=1
arch=('x86_64')
options=('!strip' '!debug')
install=fixture.install
package() { install -Dm644 "$startdir/padding" "$pkgdir/opt/pacpin-ui-progress-large/padding"; }
''')
        run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=large)
        build = self.root / "progress-native"
        build.mkdir()
        for filename in ["PKGBUILD", "fixture.install", "padding"]:
            shutil.copyfile(self.root / "native-2" / filename, build / filename)
        with (build / "PKGBUILD").open("a") as stream:
            stream.write("\ndepends=('pacpin-ui-progress-large')\n")
        run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=build)
        artifacts = [next(large.glob("*.pkg.tar.zst")), next(build.glob("*.pkg.tar.zst"))]
        original_cfg = self.cfg.read_text()
        original_delay = SlowRepository.download_delay
        try:
            SlowRepository.download_delay = .16
            for background in [False, True]:
                self.seed(source=background)
                subprocess.run(["sudo", "-n", "pacman", "-R", "--noconfirm", "pacpin-ui-progress-large"], capture_output=True)
                # Other suites use a fresh isolated ALPM database in the same
                # live root. Remove only this fixture's unowned padding file.
                padding = Path("/opt/pacpin-ui-progress-large/padding")
                if padding.exists():
                    run("sudo", "-n", "rm", "--", str(padding))
                    run("sudo", "-n", "rmdir", "--", str(padding.parent))
                self.publish(2)
                self.catalog(2)
                for archive in artifacts:
                    shutil.copyfile(archive, self.repo / archive.name)
                run("repo-add", str(self.repo / "pacpin-ui.db.tar.gz"), *[str(self.repo / archive.name) for archive in artifacts])
                run("sudo", "-n", "pacman", "-Syy", "--noconfirm")
                name = "native-aggregate-background" if background else "native-aggregate-foreground"
                if background:
                    orphan = self.root / "db/local/pacpin-ui-orphan-progress-1-1"
                    orphan.mkdir()
                    (orphan / "desc").write_text("%NAME%\npacpin-ui-orphan-progress\n\n%VERSION%\n1-1\n\n%DESC%\nAggregate download review fixture\n\n%REASON%\n1\n\n%SIZE%\n1\n\n")
                    (orphan / "files").write_text("%FILES%\n\n")
                    self.cfg.write_text(original_cfg.replace("smart_orphans = false", "smart_orphans = true"))
                self.case(name, ["-Su"] if background else ["-Su", "--noconfirm"], native=True, source=background, confirm=background, selection=background, selection_wait=background, width=80)
                frames = json.loads((self.root / f"{name}.frames.json").read_text())
                observed = []
                for frame in frames:
                    match = re.search(r"(?:^|\n)Pacman: Downloading packages \((\d+)%\)", frame)
                    if match:
                        observed.append(int(match[1]))
                if not any(0 < percent < 100 for percent in observed):
                    self.results[-1]["failures"].append("aggregate native download percentage not observed")
                self.results[-1]["aggregate_download_percentages"] = sorted(set(observed))
                # At least one completed package must coexist with an
                # aggregate installation percent below 100 in a two-pkg plan.
                installed = next((frame for frame in frames if "Pacman: Installing packages (50%, 1/2)" in frame), None)
                if installed is None:
                    self.results[-1]["failures"].append("aggregate installation counter not observed")
                else:
                    (self.root / f"{name}.installation.screen.txt").write_text(installed)
                download = next((frame for frame in frames if re.search(r"Pacman: Downloading packages \([1-9]\d?%\)", frame)), None)
                if download:
                    (self.root / f"{name}.download.screen.txt").write_text(download)
                print(json.dumps(self.results[-1]), flush=True)
        finally:
            self.cfg.write_text(original_cfg)
            SlowRepository.download_delay = original_delay

    def audit_cases(self, helpers):
        # Native solver expansion uses real small packages, including two new
        # dependencies; verify full-plan counts and the final summary position.
        self.seed(source=False)
        self.publish(2)
        artifacts = []
        for name in ["pacpin-ui-dep-a", "pacpin-ui-dep-b"]:
            build = self.root / name
            build.mkdir()
            (build / "PKGBUILD").write_text(f"pkgname={name}\npkgver=1\npkgrel=1\narch=('x86_64')\noptions=('!strip' '!debug')\npackage() {{ install -Dm755 /usr/bin/true \"$pkgdir/opt/{name}/app\"; }}\n")
            run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=build)
            artifacts.append(next(build.glob("*.pkg.tar.zst")))
        build = self.root / "native-with-deps"
        build.mkdir()
        for filename in ["PKGBUILD", "fixture.install", "padding"]:
            shutil.copyfile(self.root / "native-2" / filename, build / filename)
        with (build / "PKGBUILD").open("a") as output:
            output.write("\ndepends=('pacpin-ui-dep-a' 'pacpin-ui-dep-b')\n")
        run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=build)
        artifacts.append(next(build.glob("*.pkg.tar.zst")))
        for artifact in artifacts:
            shutil.copyfile(artifact, self.repo / artifact.name)
        run("repo-add", str(self.repo / "pacpin-ui.db.tar.gz"), *[str(self.repo / p.name) for p in artifacts])
        run("sudo", "-n", "pacman", "-Syy", "--noconfirm")
        self.case("native-dependency-summary", ["-Su", "--noconfirm"], native=True)
        raw = transcript(self.root / "native-dependency-summary.raw.txt")
        assert "1 upgrades + 2 new dependencies" in raw
        assert raw.index("New Dependencies") < raw.index("Packages to Change")
        assert "3 (includes 2 new deps)" in raw
        dependency_names = run("pacman", "-Qdq").splitlines()
        assert "pacpin-ui-dep-a" in dependency_names and "pacpin-ui-dep-b" in dependency_names
        run("sudo", "-n", "pacman", "-R", "--noconfirm", "pacpin-ui-native", "pacpin-ui-dep-a", "pacpin-ui-dep-b")

        self.seed(source=False)
        self.publish(2)
        # Real successful cache cleanup, including an empty ALPM sandbox dir.
        empty = self.root / "cache/download-pacpin-empty"
        empty.mkdir()
        os.utime(empty, (time.time() - 180, time.time() - 180))
        self.case("native-no-cache", ["-Su", "--noconfirm", "--no-cache"], native=True)
        raw = transcript(self.root / "native-no-cache.raw.txt")
        assert "Native cache cleaned successfully" in raw
        assert not list((self.root / "cache").glob("*.pkg.tar.*"))
        assert not empty.exists()

        # A nonzero cache command must not turn a successful upgrade into a
        # successful overall request or print a false cleanup tick.
        self.seed(source=False)
        self.publish(2)
        sudo = helpers / "sudo"
        sudo.write_text('''#!/bin/bash
if [ "${1##*/}" = pacman ] && [ "$2" = -Scc ]; then
  echo 'PACPIN_INJECTED_CACHE_FAILURE' >&2
  exit 255
fi
exec /usr/bin/sudo "$@"
''')
        sudo.chmod(0o755)
        original_path = self.env["PATH"]
        self.env["PATH"] = f"{helpers}:{original_path}"
        try:
            self.case("native-cache-cleanup-failure", ["-Su", "--noconfirm", "--no-cache"], native=True, success=False, expected_error="Native cache cleanup failed")
            raw = transcript(self.root / "native-cache-cleanup-failure.raw.txt")
            assert "cache cleaned successfully" not in raw
            assert run("pacman", "-Q", "pacpin-ui-native").strip().endswith("2.0.0-1")
        finally:
            self.env["PATH"] = original_path
            sudo.unlink()

        # RootDir capacity fixture, not a filled filesystem: a tiny empty
        # tmpfs rejects the plan before any privileged package operation.
        self.seed(source=False)
        self.publish(2)
        small_root = self.root / "small-root"
        small_root.mkdir()
        run("sudo", "-n", "mount", "-t", "tmpfs", "-o", "size=16m", "tmpfs", str(small_root))
        pacman_conf = helpers / "pacman-conf"
        pacman_conf.write_text(f'#!/bin/sh\nif [ "$1" = RootDir ]; then printf "%s\\n" {shlex.quote(str(small_root))}; else exec /usr/bin/pacman-conf "$@"; fi\n')
        pacman_conf.chmod(0o755)
        self.env["PATH"] = f"{helpers}:{original_path}"
        try:
            self.case("native-storage-refusal", ["-Su", "--noconfirm", "--no-cache"], success=False, expected_error="Insufficient native transaction space")
            assert run("pacman", "-Q", "pacpin-ui-native").strip().endswith("1.0.0-1")
            assert not list((self.root / "cache").glob("*.pkg.tar.*"))
        finally:
            self.env["PATH"] = original_path
            pacman_conf.unlink()
            run("sudo", "-n", "umount", str(small_root))

        before_menu_config = self.cfg.read_bytes()
        self.case("repository-menu-narrow-cancel", ["repos"], repo_menu=True, width=32)
        assert self.cfg.read_bytes() == before_menu_config

        # More orphans than one viewport. These entries exist only in this
        # isolated guest ALPM database; the selector chooses None.
        original_cfg = self.cfg.read_text()
        self.seed()
        self.publish(2)
        self.catalog(2)
        for index in range(25):
            directory = self.root / f"db/local/pacpin-ui-orphan-{index:02}-1-1"
            directory.mkdir()
            (directory / "desc").write_text(f"%NAME%\npacpin-ui-orphan-{index:02}\n\n%VERSION%\n1-1\n\n%DESC%\nComplete optional inventory fixture\n\n%REASON%\n1\n\n%SIZE%\n1\n\n")
            (directory / "files").write_text("%FILES%\n\n")
        self.cfg.write_text(original_cfg.replace("smart_orphans = false", "smart_orphans = true"))
        original_delay = SlowRepository.download_delay
        try:
            # Hold the real native download open well past Enter. The original
            # 1 MiB fixture could finish before the selector's 0.6-second review.
            SlowRepository.download_delay = 0.35
            self.case("orphan-selection-download-and-enter", ["-Su"], native=True, source=True, confirm=True, selection=True, selection_wait=True, width=40)
            screen = (self.root / "orphan-selection-download-and-enter.screen.txt").read_text()
            for index in range(25):
                assert f"pacpin-ui-orphan-{index:02}" in screen
            assert all(path.exists() for path in (self.root / "db/local").glob("pacpin-ui-orphan-*"))
            self.seed()
            self.publish(2)
            self.catalog(2)
            SlowRepository.download_delay = original_delay
            self.case("orphan-selection-download-already-finished", ["-Su", "--autoremove"], native=True, source=True, confirm=True, selection=True, selection_delay=2, width=80)
            completed_raw = transcript(self.root / "orphan-selection-download-already-finished.raw.txt")
            assert "Waiting for repository downloads to finish..." not in completed_raw
            self.seed()
            self.publish(2)
            self.catalog(2)
            SlowRepository.download_delay = 0.35
            self.case("orphan-selection-wait-interrupted", ["-Su", "--autoremove"], native=True, source=True, confirm=True, selection=True, selection_wait=True, selection_wait_cancel=True, success=False, width=40)
            assert run("pacman", "-Q", "pacpin-ui-native").strip().endswith("1.0.0-1")
            assert not (self.root / "db/db.lck").exists(), "interrupted post-selection wait retained its lock"
            self.seed()
            self.publish(2)
            self.catalog(2)
            # Add a newly detected safe orphan descriptor to enter raw-mode
            # review again before cancelling.
            orphan = self.root / "db/local/pacpin-ui-orphan-cancel-1-1"
            orphan.mkdir()
            (orphan / "desc").write_text("%NAME%\npacpin-ui-orphan-cancel\n\n%VERSION%\n1-1\n\n%REASON%\n1\n\n%SIZE%\n1\n\n")
            (orphan / "files").write_text("%FILES%\n\n")
            self.case("orphan-selection-interrupted", ["-Su"], native=True, source=True, confirm=True, selection_cancel=True, success=False, width=40)
            assert run("pacman", "-Q", "pacpin-ui-native").strip().endswith("1.0.0-1")
            assert not (self.root / "db/db.lck").exists(), "interrupted native prefetch retained its lock"
        finally:
            SlowRepository.download_delay = original_delay
            self.cfg.write_text(original_cfg)
            for path in (self.root / "db/local").glob("pacpin-ui-orphan-*"):
                shutil.rmtree(path)

        # Real pacman write failure on a tiny tmpfs containing only the
        # disposable fixture's files. No host or guest root exhaustion.
        self.seed()
        self.publish(2)
        self.catalog(2)
        # Read-only destinations can produce only nonfatal unlink warnings.
        # Permit creation, then exhaust this mount while writing the 1 MiB file.
        run("sudo", "-n", "mount", "-t", "tmpfs", "-o", "size=64k,nr_inodes=128", "pacpin-extraction-test", "/opt/pacpin-ui-native")
        before_failure_config = Path("/etc/pacman.conf").read_bytes()
        failure_config = self.root / "pacman-extraction-failure.conf"
        failure_config.write_bytes(before_failure_config.replace(b"CheckSpace\n", b""))
        try:
            # Otherwise ALPM refuses the small fixture in its own space
            # check before extraction. Disable CheckSpace only for this case.
            run("sudo", "-n", "cp", str(failure_config), "/etc/pacman.conf")
            self.case("native-real-extraction-failure", ["-Su", "--noconfirm", "--no-cache"], native=True, source=True, success=False, expected_error="Some packages or files may already have changed")
            raw = transcript(self.root / "native-real-extraction-failure.raw.txt")
            assert "could not extract" in raw
            assert "Native cache cleanup skipped" in raw and "External upgrades skipped" in raw
            assert list((self.root / "cache").glob("*.pkg.tar.zst"))
            assert run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1")
        finally:
            failure_config.write_bytes(before_failure_config)
            run("sudo", "-n", "cp", str(failure_config), "/etc/pacman.conf")
            run("sudo", "-n", "umount", "/opt/pacpin-ui-native")

        # Native commit writes a file and returns an error: controlled failure
        # wrapper, not a real host disk exhaustion. Sources must be skipped;
        # cached archives must survive and no cleanup success may be printed.
        self.seed()
        self.publish(2)
        self.catalog(2)
        sudo = helpers / "sudo"
        sudo.write_text(f'''#!/bin/bash
if [ "$1" = pacman ] && [ "$2" = -S ]; then
  /usr/bin/sudo -n pacman -Sw --needed --noconfirm pacpin-ui-native || exit $?
  /usr/bin/sudo -n /usr/bin/touch {shlex.quote(str(self.root / "partial-write"))}
  echo 'error: could not extract fixture (Write failed)' >&2
  echo 'Errors occurred, no packages were upgraded.' >&2
  exit 23
fi
exec /usr/bin/sudo "$@"
''')
        sudo.chmod(0o755)
        original_path = self.env["PATH"]
        self.env["PATH"] = f"{helpers}:{original_path}"
        try:
            self.case("native-commit-failure-no-cache", ["-Su", "--noconfirm", "--no-cache"], native=True, source=True, success=False, expected_error="Some packages or files may already have changed")
            raw = transcript(self.root / "native-commit-failure-no-cache.raw.txt")
            assert "Native cache cleanup skipped" in raw and "External upgrades skipped" in raw
            assert "cache cleaned successfully" not in raw
            assert (self.root / "partial-write").exists()
            assert list((self.root / "cache").glob("*.pkg.tar.zst"))
            assert run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1")
        finally:
            self.env["PATH"] = original_path
            sudo.unlink()

    def auxiliary_failure_cases(self, helpers):
        original_path = self.env["PATH"]
        self.env["PATH"] = f"{helpers}:{original_path}"
        try:
            # These adapter commands are deliberately simulated, including
            # actual signal termination of the fixture child process.
            for filename, script in {
                "flatpak": '#!/bin/sh\ncase "$1" in run) kill -TERM $$;; *) exit 0;; esac\n',
                "pipx": '#!/bin/sh\ncase "$1" in run) kill -TERM $$;; *) exit 0;; esac\n',
                "nix": '#!/bin/sh\nif [ "$1" = --version ]; then exit 0; else exit 1; fi\n',
                "nix-shell": '#!/bin/sh\nkill -TERM $$\n',
            }.items():
                fixture = helpers / filename
                fixture.write_text(script)
                fixture.chmod(0o755)
            for provider, target in [("flatpak", "org.pacpin.Fixture"), ("pipx", "fixture"), ("nix", "fixture")]:
                self.case(f"ephemeral-{provider}-signal-failure", ["run", f"{provider}/{target}"], success=False)
        finally:
            self.env["PATH"] = original_path
            for filename in ["flatpak", "pipx", "nix", "nix-shell"]:
                (helpers / filename).unlink(missing_ok=True)

        self.seed(source=False, native=False)
        orphan = self.root / "db/local/pacpin-ui-orphan-failure-1-1"
        orphan.mkdir()
        (orphan / "desc").write_text("%NAME%\npacpin-ui-orphan-failure\n\n%VERSION%\n1-1\n\n%REASON%\n1\n\n%SIZE%\n1\n\n")
        (orphan / "files").write_text("%FILES%\n\n")
        sudo = helpers / "sudo"
        sudo.write_text('#!/bin/sh\nif [ "$1" = pacman ] && [ "$2" = -Rn ]; then echo PACPIN_ORPHAN_REMOVAL_FAILURE >&2; exit 23; fi\nexec /usr/bin/sudo "$@"\n')
        sudo.chmod(0o755)
        self.env["PATH"] = f"{helpers}:{original_path}"
        try:
            self.case("orphan-removal-failure", ["orphans", "-c", "--noconfirm"], success=False, expected_error="PACPIN_ORPHAN_REMOVAL_FAILURE")
            assert orphan.exists()
        finally:
            self.env["PATH"] = original_path
            sudo.unlink()
            shutil.rmtree(orphan)

    def auth_cases(self):
        # Override only guest pacman authentication. Guest setup commands stay
        # passwordless so cleanup is possible even after a failed assertion.
        policy = Path("/etc/sudoers.d/zz-pacpin-ui-auth")
        if policy.exists():
            raise AssertionError("Refusing to overwrite an existing sudoers test policy")
        fixture = self.root / "sudoers-auth"
        for name, native, source, renewal in [("auth-native-only", True, False, False), ("auth-sources-only", False, True, False), ("auth-native-and-sources", True, True, False), ("auth-native-sources-no-cache", True, True, False), ("auth-ticket-renewal", True, True, True)]:
            self.seed(native=native, source=source)
            self.publish(2 if native else 1)
            self.catalog(2 if source else 1)
            timeout = 1.5 if renewal else 15
            fixture.write_text(f"Defaults:garuda timestamp_type=tty,timestamp_timeout={timeout},verifypw=all\ngaruda ALL=(root) PASSWD: /usr/bin/pacman\n")
            run("sudo", "-n", "visudo", "-cf", str(fixture))
            hook = self.root / "fixture-hook.sh"
            original_hook = hook.read_text()
            original_path = self.env["PATH"]
            if renewal:
                hook.write_text(original_hook + "sleep 100\n")
                tools = self.root / "renewal-tools"
                tools.mkdir()
                log = shlex.quote(str(self.root / "sudo-renewal.log"))
                wrapper = tools / "sudo"
                wrapper.write_text(f'''#!/bin/sh
if [ "$*" = "-n -v" ]; then
  printf 'stdin=%s session=%s\\n' "$(readlink /proc/$$/fd/0)" "$(ps -o sid= -p $$)" >> {log}
  /usr/bin/sudo "$@" 2>> {log}
  result=$?
  printf 'validation_exit=%s\\n' "$result" >> {log}
  exit "$result"
fi
exec /usr/bin/sudo "$@"
''')
                wrapper.chmod(0o755)
                self.env["PATH"] = f"{tools}:{original_path}"
            try:
                run("sudo", "-n", "install", "-m", "0440", str(fixture), str(policy))
                probe = subprocess.run(["sudo", "-k", "-n", "pacman", "--version"], capture_output=True, text=True)
                if probe.returncode == 0:
                    raise AssertionError("Guest pacman is still passwordless; authentication test invalid")
                probe = subprocess.run(["sudo", "-n", "-v"], capture_output=True, text=True)
                if probe.returncode == 0:
                    raise AssertionError("sudo validation created credentials without authentication; test invalid")
                args = ["-Syu", "--noconfirm"] + (["--no-cache"] if name.endswith("no-cache") else [])
                self.case(name, args, native=native, source=source, password=True, resize=True, repeat=2 if name == "auth-native-and-sources" else 1, timeout=150 if renewal else None, password_prompt="[sudo] password for garuda:" if name == "auth-native-only" else "PACPIN_AUTH_PASSWORD:", resize_password=name == "auth-native-only")
                if name.endswith("no-cache"):
                    assert "Native cache cleaned successfully" in transcript(self.root / f"{name}.raw.txt")
            finally:
                run("sudo", "-n", "rm", "--", str(policy))
                hook.write_text(original_hook)
                self.env["PATH"] = original_path
        # AUR checks use the usual timeout with the same command-specific policy.
        fixture.write_text("Defaults:garuda timestamp_type=tty,timestamp_timeout=15,verifypw=all\ngaruda ALL=(root) PASSWD: /usr/bin/pacman\n")
        self.seed()
        self.publish(1)
        self.catalog(2)
        tools = self.root / "reset-failure-tools"
        tools.mkdir()
        wrapper = tools / "sudo"
        wrapper.write_text('#!/bin/sh\nif [ "$1" = "-k" ]; then echo PACPIN_SESSION_RESET_FAILURE >&2; exit 23; fi\nexec /usr/bin/sudo "$@"\n')
        wrapper.chmod(0o755)
        original_path = self.env["PATH"]
        self.env["PATH"] = f"{tools}:{original_path}"
        try:
            self.case("auth-session-failure", ["-Su", "--noconfirm"], source=True, success=False, expected_error="Cannot start a fresh sudo session")
            assert run("pacman", "-Q", "pacpin-ui-source").strip().endswith("1.0.0-1")
        finally:
            self.env["PATH"] = original_path

    def remaining_suite(self, helpers):
        self.other_provider_cases(helpers)
        self.seed()
        self.publish(1)
        self.catalog(2)
        redirected = run(str(self.binary), "-Su", "--noconfirm", env=self.env)
        assert "\x1b[1;" not in redirected and "Native/AUR:" not in redirected
        (self.root / "redirected.txt").write_text(redirected)
        self.results.append({"case": "redirected-source-only", "failures": []})
        if self.test_aur:
            self.aur_cases()
        self.advisory()

    def aur_cases(self, password=False):
        # Real paru and makepkg, real AUR hello recipe and GNU release. Only
        # the older starting package is a fixture; it has its own /opt path.
        build = self.root / "aur-old"
        build.mkdir()
        (build / "PKGBUILD").write_text("pkgname=hello\npkgver=0.0.0\npkgrel=1\narch=('x86_64')\npackage() { install -Dm644 /etc/os-release \"$pkgdir/opt/pacpin-ui-aur/placeholder\"; }\n")
        run("makepkg", "--nodeps", "--noconfirm", "--nocolor", cwd=build)
        old = next(build.glob("*.pkg.tar.zst"))
        paru_dir = self.root / "config/paru"
        paru_dir.mkdir()
        (paru_dir / "paru.conf").write_text("[options]\nSkipReview\n")
        self.cfg.write_text(self.cfg.read_text().replace('[pins]', '[pins]\nhello = "aur"'))
        try:
            for name, native, source in [("aur-only", False, False), ("aur-and-sources", False, True), ("native-aur-and-sources", True, True), ("aur-install", False, False), ("aur-no-cache", False, False)]:
                self.seed(source=source, native=native)
                self.publish(2 if native else 1)
                self.catalog(2 if source else 1)
                run("sudo", "-n", "pacman", "-U", "--noconfirm", str(old))
                args = ["-S", "aur/hello", "--noconfirm"] if name == "aur-install" else ["-Su", "--noconfirm"]
                native_marker = self.root / "cache/pacpin-ui-cache-marker-1-1-x86_64.pkg.tar.zst"
                if name == "aur-no-cache":
                    args.append("--no-cache")
                    native_marker.write_text("Synthetic native archive preservation marker")
                policy = Path("/etc/sudoers.d/zz-pacpin-ui-auth")
                if password:
                    if policy.exists():
                        raise AssertionError("Refusing to overwrite an existing sudoers test policy")
                    run("sudo", "-n", "install", "-m", "0440", str(self.root / "sudoers-auth"), str(policy))
                try:
                    self.case(("auth-" if password else "") + name, args, native=native, source=source, aur=True, password=password)
                    if name == "aur-no-cache":
                        raw = transcript(self.root / (("auth-" if password else "") + name + ".raw.txt"))
                        assert "AUR cache cleaned successfully" in raw
                        assert "Purging native" not in raw and native_marker.exists()
                finally:
                    if password:
                        run("sudo", "-n", "rm", "--", str(policy))
        finally:
            subprocess.run(["sudo", "-n", "pacman", "-R", "--noconfirm", "hello"], capture_output=True)

    def integration_cases(self, helpers):
        # Controlled provider fixture: exercise the real Flatpak adapter without
        # downloading applications. Explicitly records its simulated nature.
        flatpak = helpers / "flatpak"
        flatpak.write_text('#!/bin/sh\ncase "$1" in\n--version) echo "Flatpak fixture";;\nremote-ls) if [ -f "' + str(self.root / 'flatpak-update') + '" ]; then printf "UI Fixture\\torg.pacpin.UiFixture\\t2\\tfixture\\n"; fi;;\nupdate) echo "PACPIN_FLATPAK_FIXTURE_UPDATED"; sleep 1;;\nesac\n')
        flatpak.chmod(0o755)
        original_cfg, original_env = self.cfg.read_text(), dict(self.env)
        self.cfg.write_text(original_cfg.replace("integrations = false", "integrations = true").replace("flatpak = false", "flatpak = true"))
        self.env["PATH"] = f"{helpers}:{self.env['PATH']}"
        self.env["HOME"] = str(self.root / "fixture-home")
        Path(self.env["HOME"]).mkdir()
        try:
            self.seed(source=False)
            self.publish(2)
            self.case("native-with-integration-no-updates", ["-Su", "--noconfirm"], native=True)
            self.seed(source=False)
            self.publish(2)
            (self.root / "flatpak-update").touch()
            shutil.rmtree(Path(self.env["HOME"]) / ".cache", ignore_errors=True)
            self.case("native-and-provider-without-prefetch", ["-Su", "--noconfirm"], native=True)
            assert "PACPIN_FLATPAK_FIXTURE_UPDATED" in (self.root / "native-and-provider-without-prefetch.raw.txt").read_text()
            frames = json.loads((self.root / "native-and-provider-without-prefetch.frames.json").read_text())
            assert any(re.search(r"(?:^|\n)Flatpak:", frame) for frame in frames)
        finally:
            self.env = original_env
            self.cfg.write_text(original_cfg)
            flatpak.unlink()

    def advisory(self):
        # Real mmap of a deleted fixture library, observed by restart-check.
        library = Path("/usr/lib/pacpin-ui-test/libupower-glib.so.3.1.2")
        run("sudo", "-n", "mkdir", "-p", str(library.parent))
        run("sudo", "-n", "cp", "/usr/lib/libz.so.1", str(library))
        units = [("pacpin-ui-org.gnome.Shell.Notifications@0.service", True),
                 ("pacpin-ui-org.gnome.SettingsDaemon.Power.service", True),
                 ("pacpin-ui-system.service", False)]
        try:
            for unit, user in units:
                ready = self.root / (unit + ".ready")
                code = f"import ctypes,time,pathlib; lib=ctypes.CDLL({str(library)!r}); pathlib.Path({str(ready)!r}).touch(); time.sleep(120)"
                argv = ["systemd-run", "--collect", f"--unit={unit}"]
                if user:
                    argv.append("--user")
                else:
                    argv = ["sudo", "-n", *argv, "--property=User=garuda", "--property=Group=garuda"]
                run(*argv, "/usr/bin/python", "-c", code)
                deadline = time.monotonic() + 5
                while not ready.exists():
                    if time.monotonic() > deadline:
                        raise AssertionError(f"library fixture did not start: {unit}")
                    time.sleep(0.05)
            run("sudo", "-n", "rm", "--", str(library))
            for width in [80, 40, 32]:
                name = f"restart-advisory-{width}"
                self.case(name, ["restart-check", "--pkexec"], width=width)
                raw = (self.root / f"{name}.raw.txt").read_text()
                clean = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", raw).replace("\r", "")
                assert "Library:" in clean and "libupower-glib" in clean
                assert "Log out and back in" in clean
                assert "pkexec systemctl restart" in clean
                assert "systemctl --user restart" in clean
                assert "'pacpin-ui-org.gnome.SettingsDaemon.Power.service'" in clean
                assert "'pacpin-ui-system.service'" in clean
            self.seed(source=False)
            self.publish(2)
            self.case("cleanup-before-restart-advisory", ["-Su", "--noconfirm", "--no-cache"], native=True)
            clean = transcript(self.root / "cleanup-before-restart-advisory.raw.txt")
            assert clean.index("Native cache cleaned successfully") < clean.index("Post-Upgrade Restart Advisory")
        finally:
            for unit, user in units:
                argv = ["systemctl", "--user", "stop", unit] if user else ["sudo", "-n", "systemctl", "stop", unit]
                subprocess.run(argv, capture_output=True)

    def other_provider_cases(self, helpers):
        original_cfg, original_env = self.cfg.read_text(), dict(self.env)
        nix = helpers / "nix"
        nix_env = helpers / "nix-env"
        pipx = helpers / "pipx"
        nix.write_text('#!/bin/sh\nif [ "$1" = "--version" ]; then echo "nix fixture"; else echo "PACPIN_NIX_FIXTURE_UPDATED"; sleep 1; fi\n')
        nix_env.write_text('#!/bin/sh\necho "pacpin-ui-nix-fixture"\n')
        pipx.write_text('#!/bin/sh\ncase "$1" in\n--version) echo "pipx fixture";;\nlist) printf \'{"venvs":{"pip":{"metadata":{"main_package":{"package_version":"0.0.0"}}}}}\\n\';;\nupgrade-all) echo "PACPIN_PIPX_FIXTURE_UPDATED"; sleep 1;;\nesac\n')
        for helper in [nix, nix_env, pipx]:
            helper.chmod(0o755)
        self.env["PATH"] = f"{helpers}:{self.env['PATH']}"
        try:
            for name, label, marker in [("nix", "Nix", "PACPIN_NIX_FIXTURE_UPDATED"), ("pipx", "Pipx", "PACPIN_PIPX_FIXTURE_UPDATED")]:
                self.cfg.write_text(original_cfg.replace("integrations = false", "integrations = true").replace(f"{name} = false", f"{name} = true"))
                self.seed(source=False, native=False)
                self.publish(1)
                self.case(f"{name}-provider-only", ["-Su", "--noconfirm"])
                raw = (self.root / f"{name}-provider-only.raw.txt").read_text()
                frames = json.loads((self.root / f"{name}-provider-only.frames.json").read_text())
                assert marker in raw
                assert any(re.search(rf"(?:^|\n){label}:", frame) for frame in frames)
        finally:
            self.env = original_env
            self.cfg.write_text(original_cfg)
            for helper in [nix, nix_env, pipx]:
                helper.unlink()

    def close(self):
        restore = self.root / "pacman.original.conf"
        restore.write_bytes(self.original)
        run("sudo", "-n", "cp", str(restore), "/etc/pacman.conf")
        self.server.shutdown()


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--socket", default="unix:/tmp/pacpin-ui-kitty.sock")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--baseline", action="store_true")
    parser.add_argument("--aur", action="store_true", help="Also build the small real AUR hello package using paru (requires Internet)")
    parser.add_argument("--section", choices=["all", "remaining", "auth", "audit", "progress"], default="all", help="Run all, remaining, real sudo authentication, transaction audit, or aggregate progress checks")
    args = parser.parse_args()
    if os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or 'ID=garuda' not in Path("/etc/os-release").read_text() or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay" or run("systemd-detect-virt").strip() != "oracle":
        raise SystemExit("This verifier requires PACPIN_DISPOSABLE_VM=1 inside the disposable Garuda VM")
    if os.geteuid() == 0:
        raise SystemExit("Run as the regular live user, never root")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        raise SystemExit("Use a fresh empty output directory to preserve earlier evidence and config backups")
    checks = None
    try:
        checks = Checks(args.binary, args.output, args.socket, args.baseline, args.aur, args.section)
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    failures = sum(bool(result["failures"]) for result in checks.results)
    if failures and not args.baseline:
        raise SystemExit(f"{failures} UI scenarios failed; inspect {args.output}")


if __name__ == "__main__":
    main()
