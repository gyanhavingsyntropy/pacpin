import argparse
import json
import os
from pathlib import Path
from verify_upgrade_ui_vm import Checks, run, serialized_verifier

class FinalChecks(Checks):
    def suite(self):
        self.seed()
        self.publish(1)
        self.catalog(2)
        self.case("final-source-cancellation-cleans-registry", ["-Su", "--noconfirm"], source=True, success=False, cancel=True, width=40)
        if (self.root / "db/db.lck").exists():
            self.results[-1]["failures"].append("native lock remains after interruption")
        self.seed()
        self.publish(2)
        self.catalog(2)
        self.case("final-mixed-upgrade-after-cancellation", ["-Su", "--no-cache", "--noconfirm"], native=True, source=True, width=40)
        self.progress_cases()

@serialized_verifier
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("PACPIN_DISPOSABLE_VM") != "1" or 'ID=garuda' not in Path("/etc/os-release").read_text() or run("systemd-detect-virt").strip() != "oracle" or run("findmnt", "-n", "-o", "FSTYPE", "/").strip() != "overlay" or os.geteuid() == 0:
        raise SystemExit("Only run as the regular user in the disposable Garuda live VM")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        raise SystemExit("Use a fresh output directory")
    checks = None
    try:
        checks = FinalChecks(args.binary, args.output, "unix:/tmp/pacpin-ui-kitty.sock", False, False, "all")
        checks.suite()
    finally:
        if checks:
            (args.output / "results.json").write_text(json.dumps(checks.results, indent=2))
            checks.close()
        elif (args.output / "pacman.original.conf").exists():
            run("sudo", "-n", "cp", str(args.output / "pacman.original.conf"), "/etc/pacman.conf")
    if any(result["failures"] for result in checks.results):
        raise SystemExit("Final execution cases failed")

if __name__ == "__main__":
    main()
