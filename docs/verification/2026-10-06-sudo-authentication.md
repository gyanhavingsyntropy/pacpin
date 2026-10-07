# Sudo authentication verification — 6 October 2026

## Requested behavior and cause

Each interactive sudo operation should authenticate once per pacpin invocation,
reuse authentication during that invocation, and authenticate again on the next
invocation, even in the same terminal. Read-only commands and dry runs should
not start an authentication session.

The earlier footer runner called `setsid` and `TIOCSCTTY` for each manager.
Database refresh ran on the original terminal, while native installation and
source installation each acquired a new controlling terminal. Sudo's default
per-terminal tickets could not be reused across those stages.

## Changes

- Foreground commands inherit stdin and the controlling terminal. The renderer
  relays stdout/stderr through an output PTY without consuming authentication
  input or creating a new session.
- PTY output keeps newline processing, including while sudo temporarily changes
  the invoking terminal to raw mode. A regression assertion checks carriage
  returns and rendered native marker alignment.
- Interactive sudo operations reset only the current terminal ticket lazily
  before the first actual privileged operation. The reset happens once per
  process. AUR helper execution prepares the same session while remaining
  unprivileged.
- Failed session preparation refuses command execution through the normal
  error path, so source workspaces and terminal settings are cleaned up.
- A worker runs `sudo -n -v` every 60 seconds using the inherited input terminal
  to renew an existing ticket during long operations. It cannot prompt or read
  a password. Read-only operations, dry runs, noninteractive invocations, and
  other selected privilege tools do not start this session.
- AGENTS.md requires password-enabled VM verification and consecutive runs in
  the same terminal; the live account's usual passwordless sudo is insufficient.

## Reproduction and verification

The host uses sudo-rs 0.2.15-1. The disposable Garuda live guest uses regular
user `garuda`, real sudo 1.9.17p2,
real pacman/makepkg/paru, and Kitty 0.48.2 on the guest Xorg display `:0`.
Native and source fixtures, isolation and emulator setup are described in
[the terminal verification report](2026-10-06-terminal-ui.md).

Inside the VM:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --section auth --aur --output /tmp/pacpin-auth-final-v4

PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --output /tmp/pacpin-sudo-ui-final
```

Authentication checks temporarily require passwords only for guest pacman,
with `timestamp_type=tty` and cache-aware `verifypw=all`, and remove the guest policy in
`finally`. Setup and restoration remain possible using the live guest's usual
command permissions. Host sudoers and host packages are not modified.

The previous installed binary reproduced a second password request after
successful database refresh in the same native transaction. The verifier stops
on an extra prompt rather than supplying another password. Its first draft
mistakenly counted script's command header; prompt counting was corrected to
exclude transcript metadata before the valid reproduction.

The intermediate candidate also exposed missing carriage returns during sudo's
raw terminal mode. The initial expiry test policy used `verifypw=always`, which
forces authentication even for a current validation ticket; it was corrected
to cache-aware validation. The upstream implementations in
[lookup.c](https://github.com/sudo-project/sudo/blob/v1.9.17p2/plugins/sudoers/lookup.c)
and [check.c](https://github.com/sudo-project/sudo/blob/v1.9.17p2/plugins/sudoers/check.c)
confirm that distinction. The final release was rechecked with normal ticket
renewal behavior.

The authentication setup failure case deliberately replaces guest sudo's `-k`
operation with a fixture returning exit 23. It asserts that source installation
is refused, the old installed version survives, scratch is removed, the footer
is cleared, and terminal settings are restored. Other authentication scenarios
use real sudo; the long-operation wrapper only logs actual `sudo -n -v` results.

## Results

- `cargo test`: 239 passed, 2 ignored; the two ignored source packaging/signal
  checks were then executed explicitly with `PACPIN_TEST_BINARY`, both passed.
  Total exercised: 241 tests, zero failures.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed. Release and installed binary are byte-equal;
  the `pin` symlink still points to `pacpin`.
- Authentication results (local verification artifact):
  all nine scenarios passed. Native-only and Sources-only each prompted once;
  two consecutive combined runs on one controlling terminal prompted twice,
  once per run. A 100-second hook with a 90-second ticket lifetime still prompted
  once, with a real renewal returning status 0. Four real AUR combinations each
  prompted once. The simulated reset failure preserved old installed versions,
  cleaned source scratch and restored terminal settings.
- UI results (local verification artifact): all twenty
  scenarios passed, including ordinary native installation, source/native
  combinations, prompts, resizing, narrow terminals, checksum/packaging/native
  failures, Ctrl-C, no updates, integrations, redirected output, and restart
  advice at 80/40/32 columns. Flatpak/Nix/Pipx executables were fixtures; these
  were adapter/UI checks rather than real application upgrades.
- Baseline results (local verification artifact)
  reproduce the previous binary's second authentication request. Its footer
  also mirrored the prompt text, so the recorded text-occurrence count includes
  those copies; it is not a count of distinct password requests.
- Guest pacman configuration was restored and the temporary sudoers policy was
  absent before shutdown. `pacpin-test-vm` reached `VMState="poweroff"` at
  `2026-10-05T20:49:59Z` (6 October, 02:19:59 IST). No VirtualBox headless process
  or running VM remained.

Final tested/released/installed binary SHA-256:

```text
fcf6e45c678620b457c563e2246f3f511f0e78b393f8bfcddbb6365a5222b87c
```

Binary size: 6,770,880 bytes. The evidence directory contains rendered screens
for every scenario, the original reproduction, and the actual renewal log.
