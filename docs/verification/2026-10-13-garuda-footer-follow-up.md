# Garuda footer verification — 2026-10-13 follow-up

The disposable Garuda live VM used 3584 MiB RAM, four CPUs, real Kitty 0.48.2,
pacman/makepkg 7.1.0 and kernel 7.2.0-1-garuda. pacpin and builds ran as
`garuda`; only guest native operations were elevated. No host package
transactions were performed.

## Additional defects found

Each successive footer previously scrolled the whole screen to reserve rows
again. This could move earlier text into scrollback and leave gaps during quiet
steps. The invocation now retains one output region between operations, resizing
it only when required. Authentication releases that region; invocation exit,
including `process::exit` and cancellation, restores the terminal.

Cleanup could also stay hidden during quiet work because every sudo command
started with a deferred authorization display. The privilege boundary now uses
noninteractive validation to distinguish an already authenticated sudo session
from pending authentication. Other privilege tools retain the deferred display.
The validation inherits the existing tty and cannot ask for a password.

Phase detection now consumes every completed line in a child-output read.
A batched hook heading followed by arbitrary log lines previously left the
footer at the preceding installation phase; a regression checks both the parser
and the real native fixture's rendered hook status.

The controlled Kitty screen-engine regression verifies that a visible output
anchor stays on the same row across four footer phases without new normal
output. This supplements the actual guest CLI checks.

## Reproduction commands

Transfer the release and verifier scripts into the guest and start the private
Kitty socket using AGENTS.md. Run sequentially as the guest regular user with
`PACPIN_DISPOSABLE_VM=1`, a fresh output directory for each suite:

```sh
python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin --section auth --aur --output /tmp/auth-new
python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin --section progress --output /tmp/progress-new
python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin --aur --output /tmp/ui-new
python /tmp/verify_housekeeping_vm.py /tmp/pacpin --output /tmp/housekeeping-new
python /tmp/verify_orphan_retry_vm.py /tmp/pacpin --output /tmp/orphans-new
python /tmp/verify_runtime_flags_vm.py /tmp/pacpin --output /tmp/flags-new
python /tmp/verify_post_update_hooks_vm.py /tmp/pacpin --output /tmp/hooks-new
```

Native and Sources fixtures use actual pacman/makepkg transactions and verified
installed versions. The AUR matrix builds the real AUR `hello` package with
Paru. Integration executables and hook rebuild helpers are controlled fixtures;
their success is not an actual Flatpak/Nix/Pipx application upgrade. Failure
cases explicitly inject checksum, packaging, extraction, storage, cleanup or
helper failures. Cancellation checks inspect terminal restoration and ALPM locks.

The aggregate progress cases download unequal real archives of about 1 and
3 MiB, both in the foreground and during orphan review. Screens show Pacman's
aggregate download percentage and `50%, 1/2` installation progress while the
first package is already at 100%.

ALPM's verification bars already describe their full action; the footer keeps
those percentages directly while installation/removal still combines the
package counter and current package bar. The installed libalpm header describes
the percentage as completion of the action, consistent with its
[upstream callback documentation](https://lists.archlinux.org/pipermail/pacman-dev/2020-December/024677.html).
Unit regressions cover a 73% verification bar at package 1 of 2.

Fixture repositories now force their test database refresh with `-Syy`:
republishing metadata within one second could otherwise reuse an HTTP 304 and
leave stale archive size/dependency data. An initial progress attempt failed for
that fixture reason; its evidence is preserved and excluded from final results.
Repeated suites also remove only their known, unowned padding fixture before
seeding a fresh isolated database, avoiding a file conflict with an earlier run.
An injected orphan failure now emits an `error:` diagnostic, matching real
pacman failure classification.

## Evidence and results

Complete, unmodified rendered frames, raw streams, status files, terminal
snapshots, environment records and compact archives are retained locally in
`target/maintenance/vm-20261013/`. Evidence is exported after each suite before
the ephemeral guest is shut down. Selected normalized screens and suite results
are committed beside this report.

| Suite | Passed |
| --- | ---: |
| Native/Sources/AUR, resize, failures, selectors and advice | 41 |
| Orphans and cache | 21 |
| Pending orphan cleanup on later invocations | 12 |
| Scope, scratch and continuation flags | 16 |
| Conditional hooks and rebuild verification | 14 |
| Real sudo authentication, renewal and AUR | 11 |
| Unequal-package aggregate progress | 2 |
| **Total scenarios** | **117** |

The first five suites used build `d53684e7`, with footer continuity, quiet
authenticated stages and batched hook detection fixed. The final verification
percentage correction additionally received fresh authentication and aggregate
checks on release `74a79550`; the summary records the exact SHA for each suite.
Forced RAM exercised safe capacity refusal in this guest, rather than a
successful RAM build. Expected injected failures and cancellations count as
passing only when their failure, scope and cleanup assertions pass. Initial
failed fixture attempts are excluded.

Local verification: **288 passed, 0 failed, 2 existing explicit-only tests
ignored**; strict Clippy and release build passed. The installed system binary
matches the release byte for byte, and the `pin` symlink is preserved.

The saved cleanup screenshot (local verification artifact) shows
completed native, Sources and orphan stages alongside active native cleanup.
The suite summary (local verification artifact) and normalized screens
retain the compact evidence; complete captures remain in the local directory.

The final guest check confirmed the original pacman configuration was restored,
the temporary authentication policy was removed, and no pacman/makepkg processes
or isolated ALPM database locks remained. After exporting evidence, the guest
was shut down through `systemctl poweroff`. Host verification returned
`VMState="poweroff"`, no running VM and no VBoxHeadless process.
