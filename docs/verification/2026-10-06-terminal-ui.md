# Garuda terminal verification — 6 October 2026

## Environment and reproduction

VM access and graphical terminal setup are documented in [AGENTS.md](../../AGENTS.md).
The live guest was Garuda, regular user `garuda` (UID 1000), kernel
`7.2.0-1-garuda`, Kitty 0.48.2, 5120 MiB RAM and four CPUs. Kitty ran on the
guest Xorg display `:0`; the matching virtual console was VT 2. Rendered
screens and scrollback were read through Kitty remote control, and VirtualBox
console screenshots were inspected. This used a real terminal emulator.

Native fixtures were built with makepkg and upgraded using real pacman, with
an isolated guest database, cache, local HTTP repository, and test hook
directory. Forty native installation markers and five hook markers exercised
scrolling. Source fixtures were 8 MiB local catalog archives, verified,
packaged with real makepkg and installed with real pacman. A read interposer
slowed only the fixture archive to make download progress, resizing and
Ctrl-C observable. The packaging failure was deliberately simulated.

AUR checks used real paru, the real AUR `hello` recipe, its GNU upstream
release, and real unprivileged makepkg compilation. The starting older `hello`
package was a fixture. Flatpak, Nix, and Pipx executables were controlled
fixtures for adapter/terminal checks; they did not upgrade real applications.
Pipx update metadata came from PyPI.

The verifier restores the original guest pacman configuration in `finally`.
Its guard requires an explicit environment flag, the Garuda live overlay,
VirtualBox, a regular user, and an empty output directory. Run inside the VM:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --aur --output /tmp/pacpin-ui-review-new
```

The original installed binary was copied to guest `/tmp/pacpin-before` for:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin-before \
  --baseline --output /tmp/pacpin-ui-baseline-new
```

## Reproduced bugs and changes

- The old source-only path had no persistent Sources progress row.
- The old mixed transaction lost 17 of 40 native markers from rendered
  scrollback after source output. Its footer also said `Native/AUR` although
  no AUR helper ran. See baseline results (local verification artifact).
- Resetting the scrolling region homes the cursor. Footer teardown and signal
  restoration now save/restore that cursor and clear the reserved rows.
- Status labels identify the manager actually invoked. Ordinary native/AUR
  installs and Flatpak/Nix/Pipx upgrades use the shared renderer. Only actual
  overlapping native/AUR and Sources work gets a second Sources row.
- Prefetch capabilities are explicit and default to unsupported. Source-only
  updates are foreground work; native/source overlap begins at pacman's
  post-download stages. Workers update shared state without writing into the
  manager's terminal output.
- Foreground downloads retain terminal signal processing, resize their footer,
  and clear it after Ctrl-C. Native download failures discard source work that
  was queued but never started.
- Small windows use stacked transaction rows and wrapped summary boxes.
  Unknown source, AUR and integration sizes are marked `Unknown`; partial
  totals say `+ unknown`. Narrow source rows retain byte counts and percentages.
  Pacman keeps its stage output with `--noprogressbar` when launched in a
  window narrower than 60 columns; the shared footer remains visible.
- Restart advice stacks the name, library and action, uses the real terminal
  width and runtime privilege tool, and keeps shell arguments intact. Long
  individual command arguments soft-wrap in the terminal; names in commands
  are not split with hard newlines.

## Results

There were 27 successful recorded scenario runs across two candidates:
14 in reviewed results (local verification artifact), followed
by 13 in final results (local verification artifact).
The intermediate verifier encountered an empty pacman database, whose `-Q`
returns status 1; its version-query handling was corrected before resuming.
The final candidate also suppresses the queued Sources notice after a failed
native download. That regression was checked again on the final build.

| Scenario | Observed result |
| --- | --- |
| Sources only; native only; native and Sources | Correct scoped rows; expected installed versions; all 40 native markers retained |
| Ordinary native installation | Native footer, hooks and history retained |
| Live resize and interactive confirmation | 80×30 to 40×18; prompt answered; terminal settings restored |
| Mixed 40-column and Source-only 32-column windows | Readable summary box; scoped footer; source counts/percent preserved |
| Checksum and packaging failures | Exit 1, old source version retained, no success message, no workspace leak |
| Download interruption | Exit 130, old version retained, terminal settings restored, footer/workspace removed |
| Native download failure | Exit 1; source install skipped; final candidate clears the queued notice |
| No updates; integration with no updates | No irrelevant Sources/AUR rows |
| Native plus Flatpak fixture without prefetch support | Native then Flatpak rows; no Sources row or fake prefetch |
| Nix-only and Pipx-only adapter fixtures | Their own rows; no Native/AUR/Sources row |
| Redirected Source-only output | Successful upgrade without footer escape sequences |
| Real AUR only; AUR and Sources; all three managers | Real `hello 2.12.1-2` installed; scoped rows; native history retained |
| Ordinary real AUR installation | Real paru install with AUR footer and expected version |
| Restart advisory at 80, 40 and 32 columns | Real deleted-library mappings detected in disposable user/system services; desktop advice, `systemctl --user`, and `pkexec` commands verified |

Restart fixtures loaded a private copy of libz named
`/usr/lib/pacpin-ui-test/libupower-glib.so.3.1.2`. The file was unlinked while
the processes retained its mapping. Temporary units were stopped after checks;
no real desktop or system service was restarted.

Selected original rendered snapshots:

- Native and Sources footer (local verification artifact)
- AUR footer (local verification artifact)
- Mixed narrow transaction (local verification artifact)
- Full native/AUR/Sources history (local verification artifact)
- 32-column restart advice (local verification artifact)
- 32-column console screenshot (local verification artifact)

## Build and host sync

- `cargo test`: 236 passed, 0 failed, 2 ignored.
- Both ignored source tests explicitly enabled with `PACPIN_TEST_BINARY`
  and `TMPDIR=/var/tmp`: 2 passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed; executable size 6,813,336 bytes.
- Final build and installed binary SHA-256:
  `6702cd865162659e0db3c3b192c5235af6a45c518d8afe8b800753f965b6f1cb`.
- Atomic sync to `$HOME/.local/bin/pacpin`; `cmp` passed. The `pin`
  symlink still points to `pacpin`. No commit, push, or tag change was made.

The the VM storage server server was temporarily shut down during testing, blocking live-ISO
reads. The running guest resumed when the server returned; the remaining
AUR/advisory checks completed successfully. The guest configuration was
compared with its backup before requesting shutdown. The VM is no longer
listed in `VBoxManage list runningvms`, so it no longer holds the test RAM.
VirtualBox reports its final state as `aborted`; this was not a confirmed clean
`poweroff` state.
