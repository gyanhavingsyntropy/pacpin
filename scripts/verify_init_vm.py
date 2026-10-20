#!/usr/bin/env python3
"""Run isolated onboarding scenarios in the disposable guest's real Kitty.

No package operations or host configuration changes. The caller must start the
documented Kitty socket and pass a built pacpin binary. Captures include screen,
scrollback and parsed config assertions, not just raw pseudo-terminal output.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import time
import tomllib

parser = argparse.ArgumentParser()
parser.add_argument("binary")
parser.add_argument("--output", required=True)
args = parser.parse_args()
assert os.geteuid() != 0
assert "garuda" in Path("/etc/os-release").read_text().lower()
assert os.environ.get("PACPIN_DISPOSABLE_VM") == "1"
root = Path(args.output)
root.mkdir(parents=True, exist_ok=False)
socket = "unix:/tmp/pacpin-ui-kitty.sock"
results = []


def kitty(*argv, data=None):
    return subprocess.run(["kitty", "@", "--to", socket, *argv], input=data,
                          text=True, capture_output=True, check=True).stdout


def text(extent="all"):
    return kitty("get-text", "--extent", extent)


def send(value):
    kitty("send-text", "--stdin", data=value)


def wait(predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = text()
        if predicate(value):
            return value
        time.sleep(.08)
    raise AssertionError("Timed out waiting for terminal state:\n" + text("screen"))


def start(name, arguments="init", width=80, main=None, supplements=None):
    case = root / name
    case.mkdir()
    home = case / "home"
    home.mkdir()
    config = case / "config" / "pacpin"
    if main is not None or supplements:
        config.mkdir(parents=True)
    if main is not None:
        (config / "config.toml").write_text(main)
    for filename, body in (supplements or {}).items():
        (config / filename).write_text(body)
    # A real shell file makes it possible to detect unintended alias writes.
    (home / ".bashrc").write_text("# unchanged shell\n")
    kitty("resize-os-window", "--action", "resize", "--width", str(width), "--height", "30", "--unit", "cells")
    send("clear\n")
    time.sleep(.12)
    environment = {"HOME": str(home), "XDG_CONFIG_HOME": str(case / "config"),
                   "XDG_STATE_HOME": str(case / "state"), "XDG_CACHE_HOME": str(case / "cache")}
    command = "env " + " ".join(f"{key}={shlex.quote(value)}" for key, value in environment.items())
    command += " " + shlex.quote(str(Path(args.binary).resolve())) + " " + arguments
    command += f"; printf '\\n__INIT_DONE_%s_%s__\\n' {shlex.quote(name)} \"$?\""
    send(command + "\n")
    return case, config


def finish(name, case, expected=0, aliases=False):
    marker = re.compile(rf"^__INIT_DONE_{re.escape(name)}_(\d+)__$", re.M)
    output = wait(lambda value: marker.search(value))
    code = int(marker.search(output).group(1))
    (case / "scrollback.txt").write_text(output)
    (case / "screen.txt").write_text(text("screen"))
    assert code == expected, (name, code, output)
    shell = (case / "home" / ".bashrc").read_text()
    if aliases:
        assert shell.count('alias pin="pacpin"') == 1
        assert (case / "state" / "pacpin" / "alias_notified").exists()
    else:
        assert shell == "# unchanged shell\n"
        assert not (case / "state").exists(), "Setup must not notify/install aliases when declined"
    results.append({"name": name, "exit": code, "passed": True})
    (root / "results.json").write_text(json.dumps(results, indent=2))
    return output


def answer_preferences(count=11):
    # Keep terminal input physical and paced, including a single Enter at each
    # default. No pipe to pacpin and no pseudo-terminal wrapper around it.
    for index in range(count):
        wait(lambda value: value.rstrip().endswith(">"))
        send("\n")
        time.sleep(.1)


def review():
    wait(lambda value: "Save this configuration and create missing templates?" in re.sub(r"\s+", " ", value)
         and value.rstrip().endswith(">"))


for name, arguments, width in [("fresh", "init", 80), ("narrow", "setup", 44), ("first-launch", "", 80)]:
    case, config = start(name, arguments, width)
    answer_preferences()
    review()
    (case / "review-screen.txt").write_text(text("screen"))
    send("y\n")
    output = finish(name, case)
    parsed = tomllib.loads((config / "config.toml").read_text())
    assert parsed["options"]["privilege_tool"] == "sudo"
    assert parsed["options"]["no_cache"] is False
    assert parsed["features"]["shell_aliases"] is False
    assert parsed["features"]["integrations"] is False
    assert {"config.toml", "managers.toml", "sources.toml", "hooks.toml"}.issubset(p.name for p in config.iterdir())
    for needle in ["[1/6]", "[6/6]", "sources.toml", "hooks.toml", "--use-ram", "--keep-going", "Next steps"]:
        assert needle in re.sub(r"\s+", " ", output), (name, needle)
    validation = subprocess.run([args.binary, "config", "validate"], capture_output=True, text=True,
                                env={**os.environ, "HOME": str(case / "home"),
                                     "XDG_CONFIG_HOME": str(case / "config"),
                                     "XDG_STATE_HOME": str(case / "state"),
                                     "XDG_CACHE_HOME": str(case / "cache")})
    assert validation.returncode == 0, validation.stderr
    (case / "validation.txt").write_text(validation.stdout + validation.stderr)

for name, action in [("quit", "q\n"), ("eof", "\x04"), ("interrupt", "\x03")]:
    case, config = start(name)
    wait(lambda value: value.rstrip().endswith(">"))
    send(action)
    output = finish(name, case, 130 if name == "interrupt" else 0)
    if name != "interrupt":
        assert "Setup cancelled" in output
    assert not config.exists()

case, config = start("decline-review")
answer_preferences()
review()
send("\n")
assert "Setup cancelled" in finish("decline-review", case)
assert not config.exists()

case, config = start("invalid-answers")
wait(lambda value: value.rstrip().endswith(">"))
send("unknown-tool\n")
wait(lambda value: "Unsupported privilege tool" in value)
send("pkexec\n")
wait(lambda value: "AUR helper name or trusted absolute executable path" in re.sub(r"\s+", " ", value) and value.rstrip().endswith(">"))
send("not-a-helper\n")
wait(lambda value: "Untrusted AUR helper" in value)
send("paru\n")
wait(lambda value: "Enable configured repository pinning?" in value and value.rstrip().endswith(">"))
send("maybe\n")
wait(lambda value: "Enter yes or no" in value)
answer_preferences(9)
review()
send("y\n")
finish("invalid-answers", case)
assert tomllib.loads((config / "config.toml").read_text())["options"]["privilege_tool"] == "pkexec"

case, config = start("aliases-opt-in")
answer_preferences(7)
wait(lambda value: "Maintain a pin=pacpin alias" in value and value.rstrip().endswith(">"))
send("y\n")
time.sleep(.1)
answer_preferences(3)
review()
assert (case / "home" / ".bashrc").read_text() == "# unchanged shell\n"
send("y\n")
finish("aliases-opt-in", case, aliases=True)

case, config = start("delay-and-flatpak")
answer_preferences(4)
wait(lambda value: "Enable configured stability delays?" in value and value.rstrip().endswith(">"))
send("y\n")
wait(lambda value: "Add delay rules for known kernel packages?" in value and value.rstrip().endswith(">"))
send("y\n")
wait(lambda value: "Kernel delay in days" in value and value.rstrip().endswith(">"))
send("-1\n")
wait(lambda value: "Enter a nonnegative whole number of days" in value)
send("5\n")
time.sleep(.1)
answer_preferences(3)
wait(lambda value: "Include flatpak updates?" in value and value.rstrip().endswith(">"))
send("y\n")
time.sleep(.1)
answer_preferences(2)
review()
send("y\n")
finish("delay-and-flatpak", case)
parsed = tomllib.loads((config / "config.toml").read_text())
assert parsed["features"]["stability_delays"] is True
assert parsed["delay"] and set(parsed["delay"].values()) == {5}
assert parsed["features"]["integrations"] is True
assert parsed["integrations"] == {"flatpak": True, "nix": False, "pipx": False}

main = """# retain exact original in backup
repo_order = ['extra']
[features]
shell_aliases=false
stability_delays=true
vendor_stickiness=true
integrations=true
[options]
privilege_tool='sudo'
helper='paru'
no_cache=true
[pins]
example='extra'
[exclude]
extra=['example']
[delay]
linux=9
[packages]
pacman=['bash']
aur=['hello']
[sources.embedded]
kind='local'
url='/tmp/no-catalog.json'
packages={}
[hooks.embedded]
watch=['mutter']
action='script'
command=['/usr/bin/true']
"""
supplements = {
    "managers.toml": """# literal user-owned definitions
[managers.pacman]
adapter='pacman'
privilege_tool='pkexec'
[managers.aur]
adapter='aur'
helper='yay'
[managers.flatpak]
adapter='flatpak'
enabled=true
[managers.nix]
adapter='nix'
enabled=false
[managers.pipx]
adapter='pipx'
enabled=false
[managers.custom]
adapter='command'
enabled=true
check=['/usr/bin/false']
verify=['/usr/bin/false','{id}']
upgrade=['/usr/bin/false','{id}']
""",
    "sources.toml": "# retain recipe\n[sources.external]\nkind='local'\nurl='/tmp/no-other-catalog.json'\npackages={}\n",
    "hooks.toml": "# retain hook\n[hooks.external]\nwatch=['linux']\naction='script'\ncommand=['/usr/bin/true']\n",
}
case, config = start("reconfigure", main=main, supplements=supplements)
answer_preferences(6)  # 2 tool overrides and 3 enabled overrides skip prompts
review()
send("y\n")
output = finish("reconfigure", case)
def retains(original, updated):
    if isinstance(original, dict):
        return all(key in updated and retains(value, updated[key]) for key, value in original.items())
    return original == updated


assert retains(tomllib.loads(main), tomllib.loads((config / "config.toml").read_text()))
assert any(path.read_text() == main for path in config.glob("config.toml.backup-*.toml"))
for name, body in supplements.items():
    assert (config / name).read_text() == body
assert "owned by managers.toml" in output

case, config = start("reset", "init --reset", main="invalid = [", supplements=supplements)
answer_preferences(6)
review()
send("y\n")
finish("reset", case)
assert any(path.read_text() == "invalid = [" for path in config.glob("config.toml.backup-*.toml"))
assert tomllib.loads((config / "config.toml").read_text())["pins"] == {}
for name, body in supplements.items():
    assert (config / name).read_text() == body

for filename in ["config.toml", "sources.toml", "managers.toml", "hooks.toml"]:
    name = "conflict-" + filename.split(".")[0]
    case, config = start(name)
    answer_preferences()
    review()
    config.mkdir(parents=True)
    (config / filename).write_text("# concurrent edit\n")
    send("y\n")
    output = finish(name, case, 1)
    assert "changed during setup" in output
    assert (config / filename).read_text() == "# concurrent edit\n"
    assert not list(config.glob("config.toml.backup-*"))

for name, arguments, expected in [("bad-option", "init --pkexec", 2), ("help", "init --help", 0),
                                  ("malformed", "init", 1), ("invalid-supplement", "init --reset", 1)]:
    bad_main = "invalid = [" if name == "malformed" else None
    bad_supplement = {"hooks.toml": "invalid = ["} if name == "invalid-supplement" else None
    case, config = start(name, arguments, main=bad_main, supplements=bad_supplement)
    finish(name, case, expected)
    if bad_main:
        assert (config / "config.toml").read_text() == bad_main
    else:
        assert not (config / "config.toml").exists()

case, config = start("noninteractive", "init </dev/null")
assert "requires an input/output terminal" in finish("noninteractive", case, 1)
assert not config.exists()

case, config = start("templates-only", "config init </dev/null")
finish("templates-only", case)
assert not (config / "config.toml").exists()
assert {path.name for path in config.iterdir()} == {"managers.toml", "sources.toml", "hooks.toml"}

# A subsequent command in the same shell confirms canonical terminal input and
# output remain intact after all setup/error/cancellation paths.
send("printf '__TERMINAL_RESTORED__\\n'\n")
wait(lambda value: "\n__TERMINAL_RESTORED__\n" in value)
print(json.dumps({"passed": len(results), "output": str(root)}))
