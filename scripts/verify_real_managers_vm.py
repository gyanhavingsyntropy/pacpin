#!/usr/bin/env python3
"""Tiny real Flatpak/Nix/Pipx upgrades, ONLY in disposable Garuda/Kitty.

No manager executables are replaced. Nix uses a small local flake and the guest
daemon; Flatpak a local OSTree repo; Pipx downloads a small PyPI application.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess

from verify_upgrade_ui_vm import Checks, run, serialized_verifier, transcript


class RealChecks(Checks):
    def adapter(self, name, env):
        self.env.update(env)
        table = ", ".join(f"{key} = {json.dumps(value)}" for key, value in env.items())
        self.cfg.with_name("managers.toml").write_text(f'[managers.{name}]\nadapter = "{name}"\nenabled = true\nenv = {{ {table} }}\n')

    def external(self, name, flags=(), **kwargs):
        self.case(name, ["-Su", "--external-only", "--noconfirm", *flags], **kwargs)

    def flatpak(self):
        base = self.root / "real-flatpak"
        base.mkdir()
        self.adapter("flatpak", {"XDG_DATA_HOME": str(base / "data"), "XDG_CACHE_HOME": str(base / "cache")})
        repository = base / "repo"
        runtime = base / "runtime"
        app = base / "app"
        (runtime / "usr").mkdir(parents=True)
        (runtime / "files").mkdir()
        (runtime / "usr/fixture").write_text("tiny disposable runtime")
        (runtime / "metadata").write_text('[Runtime]\nname=org.pacpin.TestRuntime\nruntime=org.pacpin.TestRuntime/x86_64/stable\n')
        (app / "files/bin").mkdir(parents=True)
        (app / "export").mkdir()
        (app / "files/bin/fixture").write_text("version 1")
        (app / "metadata").write_text('[Application]\nname=org.pacpin.ManagerTest\nruntime=org.pacpin.TestRuntime/x86_64/stable\ncommand=fixture\n')
        run("flatpak", "build-export", "--runtime", str(repository), str(runtime), "stable", env=self.env)
        run("flatpak", "build-export", str(repository), str(app), "stable", env=self.env)
        run("flatpak", "remote-add", "--user", "--no-gpg-verify", "fixture", str(repository), env=self.env)
        reference = "app/org.pacpin.ManagerTest/x86_64/stable"
        run("flatpak", "install", "--user", "--noninteractive", "-y", "fixture", reference, env=self.env)
        old = run("flatpak", "info", "--user", "--show-commit", reference, env=self.env).strip()
        (app / "files/bin/fixture").write_text("version 2")
        run("flatpak", "build-export", str(repository), str(app), "stable", env=self.env)
        inventory = run("flatpak", "remote-ls", "--user", "--updates", "--columns=name,ref,version,origin,commit", env=self.env)
        (base / "discovery.log").write_text(inventory)
        self.external("real-flatpak-preview", ["--dry-run"])
        assert run("flatpak", "info", "--user", "--show-commit", reference, env=self.env).strip() == old
        self.external("real-flatpak-scoped-commit")
        new = run("flatpak", "info", "--user", "--show-commit", reference, env=self.env).strip()
        assert new != old
        self.external("real-flatpak-no-work")
        no_work = transcript(self.root / "real-flatpak-no-work.raw.txt")
        assert "Upgrading Flatpak" not in no_work
        assert "No updates found in the checked scope" in no_work and "packages inspected" not in no_work
        (base / "versions.json").write_text(json.dumps(dict(before=old, after=new)))

    def nix(self):
        base = self.root / "real-nix"
        base.mkdir()
        config_path = Path("/etc/nix/nix.conf")
        original = config_path.read_bytes()
        saved = base / "nix.original.conf"
        saved.write_bytes(original)
        modified = base / "nix.test.conf"
        modified.write_bytes(original + b'\nexperimental-features = nix-command flakes\nsandbox = false\ntrusted-users = root garuda\n')
        profile_link = Path.home() / ".nix-profile"
        profile_backup = base / "nix-profile.saved"
        original_profile_target = None
        profile_prepared = False
        self.adapter("nix", {"NIX_REMOTE": "daemon", "NIX_CONFIG": "experimental-features = nix-command flakes\nsandbox = false\n"})
        flake = base / "flake"
        flake.mkdir()
        def publish(version):
            (flake / "flake.nix").write_text('''{ outputs = { self }: { packages.x86_64-linux.fixture = builtins.derivation {
name = "pacpin-fixture-VERSION"; version = "VERSION"; system = "x86_64-linux";
builder = "/bin/sh"; args = [ "-c" "/usr/bin/mkdir -p $out/bin; /usr/bin/printf VERSION > $out/bin/fixture" ];
}; }; }
'''.replace("VERSION", str(version)))
            run("git", "add", "flake.nix", cwd=flake)
            run("git", "-c", "user.name=Pacpin Fixture", "-c", "user.email=fixture@localhost", "commit", "-m", f"fixture {version}", cwd=flake)
        try:
            if profile_link.exists() or profile_link.is_symlink():
                if not profile_link.is_symlink():
                    raise RuntimeError("Refusing to replace a non-symlink guest Nix profile")
                original_profile_target = os.readlink(profile_link)
                profile_backup.write_text(original_profile_target)
                profile_link.unlink()
            profile_prepared = True
            run("sudo", "-n", "cp", str(modified), str(config_path))
            run("sudo", "-n", "env", "NIX_REMOTE=local", "nix-store", "--init")
            # Stop the daemon first: a service started independently owns the
            # socket and prevents socket activation from rebinding it.
            run("sudo", "-n", "systemctl", "stop", "nix-daemon.service")
            run("sudo", "-n", "systemctl", "restart", "nix-daemon.socket")
            run("git", "init", "-q", cwd=flake)
            publish(1)
            run("nix", "profile", "install", f"git+file://{flake}#fixture", env=self.env)
            before = run("nix", "profile", "list", "--json", env=self.env)
            publish(2)
            self.external("real-nix-preview", ["--dry-run"])
            assert run("nix", "profile", "list", "--json", env=self.env) == before
            self.external("real-nix-exact-profile-element")
            after = run("nix", "profile", "list", "--json", env=self.env)
            assert json.loads(after)["elements"] != json.loads(before)["elements"]
            self.external("real-nix-no-work")
            assert "Upgrading Nix" not in transcript(self.root / "real-nix-no-work.raw.txt")
            (base / "versions.json").write_text(json.dumps(dict(before=json.loads(before), after=json.loads(after)), indent=2))
        finally:
            run("sudo", "-n", "cp", str(saved), str(config_path))
            run("sudo", "-n", "systemctl", "try-restart", "nix-daemon.service")
            if profile_prepared and profile_link.is_symlink():
                if not os.readlink(profile_link).startswith(str(self.root / "state")):
                    raise RuntimeError("Guest Nix profile changed outside the fixture")
                profile_link.unlink()
            if profile_prepared and original_profile_target is not None:
                profile_link.symlink_to(original_profile_target)

    def pipx(self):
        base = self.root / "real-pipx"
        base.mkdir()
        self.adapter("pipx", {"PIPX_HOME": str(base / "venvs"), "PIPX_BIN_DIR": str(base / "bin")})
        run("pipx", "install", "pyfiglet==0.8.post1", env=self.env)
        before = json.loads(run("pipx", "list", "--json", env=self.env))
        assert before["venvs"]["pyfiglet"]["metadata"]["main_package"]["package_version"] == "0.8.post1"
        self.external("real-pipx-preview", ["--dry-run", "--no-cache"])
        assert json.loads(run("pipx", "list", "--json", env=self.env)) == before
        self.external("real-pipx-approved-version-no-cache", ["--no-cache"])
        after = json.loads(run("pipx", "list", "--json", env=self.env))
        assert after["venvs"]["pyfiglet"]["metadata"]["main_package"]["package_version"] != "0.8.post1"
        self.external("real-pipx-no-work", ["--no-cache"])
        no_work = transcript(self.root / "real-pipx-no-work.raw.txt")
        assert "Upgrading Pipx" not in no_work
        assert "No updates found in the checked scope" in no_work and "packages inspected" not in no_work
        # Keep real Pipx creation/execution while removing network variability
        # from the success/failure cleanup cases. Upgrade cases above use PyPI.
        wheels = base / "wheels"
        wheels.mkdir()
        shared = Path(run("pipx", "environment", "--value", "PIPX_SHARED_LIBS", env=self.env).strip())
        run(str(shared / "bin/python"), "-m", "pip", "download", "--only-binary=:all:", "--dest", str(wheels), "pip", "pyfiglet", env=self.env)
        self.env.update(PIP_NO_INDEX="1", PIP_FIND_LINKS=str(wheels), PIPX_DEFAULT_BACKEND="pip")
        for case, app_args, success in [("real-pipx-run-cache-cleanup", ["--version"], True), ("real-pipx-run-failure-cache-cleanup", ["--pacpin-invalid-fixture"], False)]:
            self.case(case, ["run", "--no-cache", "pipx/pyfiglet", "--", *app_args], success=success, timeout=180)
            text = transcript(self.root / f"{case}.raw.txt")
            workspace = next(line.split("workspace ", 1)[1].split("; removed on exit", 1)[0] for line in text.splitlines() if "private Pipx workspace" in line)
            assert not Path(workspace).exists()
            assert json.loads(run("pipx", "list", "--json", env=self.env)) == after
        (base / "versions.json").write_text(json.dumps(dict(before=before, after=after), indent=2))

    def suite(self):
        self.seed(source=False, native=False)
        self.publish(1)
        for manager in (self.section,) if self.section != "all" else ("flatpak", "nix", "pipx"):
            getattr(self, manager)()


@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--socket", default="unix:/tmp/pacpin-ui-kitty.sock")
    parser.add_argument("--section", choices=["all", "flatpak", "nix", "pipx"], default="all")
    args = parser.parse_args()
    if os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or os.geteuid() == 0 or 'ID=garuda' not in Path("/etc/os-release").read_text() or run("systemd-detect-virt").strip() != "oracle" or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay":
        raise SystemExit("Requires the disposable Garuda VirtualBox live guest as its regular user")
    args.output.mkdir(exist_ok=False)
    checks = None
    try:
        checks = RealChecks(args.binary, args.output, args.socket, False, False, args.section)
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Real manager scenarios failed; inspect rendered evidence")


if __name__ == "__main__":
    main()
