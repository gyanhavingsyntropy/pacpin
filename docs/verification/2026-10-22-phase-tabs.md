# Phase tabs and current cleanup details

## Requested behavior

- Pending Orphans/Cache appear only in horizontal tabs; package managers retain
  their individual progress and pending rows during Updating.
- Completed package updates keep a tick when Orphans or Cache becomes active.
  Cleanup details replace completed package rows, for example:

  ```text
  ✔ Updating │ ✔ Orphans │ ● Cache
  Cache: Native: Cleaning package cache
  ```

## Implementation

`progress/overview.rs` derives typed tab status from actual pending, completed,
failed and skipped manager records. A download or disappearing pending row alone
cannot earn an update tick. Incomplete discovery prevents a completion tick;
active independent work remains active while failure diagnostics stay visible.
Narrow windows keep the active tab and count omitted tabs. Existing completed
Pacman/AUR rows remain during later package upgrades; cleanup uses the group tick.

`progress/stages.rs` filters only pending cleanup detail rows, preserving actual
cleanup activity and not-run diagnostics. The registry retains those phases for
tabs. Configured Flatpak/Nix/custom cache commands now use the shared Cache
transport and display their manager scope instead of reusing Updating. Actual
cleanup results set the tab state; upgrade finalization retains an aggregate
cache failure even if another manager later cleans successfully.

The PTY regression test caught lock re-entry in the initial renderer change.
Background wait now clones its status before calling the renderer; display
snapshots are collected before locking stdout. The corrected full suite passed
before installation. No package scheduling, approval or privilege policy changed.

README, AGENTS.md, architecture and transaction/UI contracts were updated.
Version remains 3.3.0-alpha.1; this is a development refinement of that release.

## Verification

- `cargo test`: **406 passed, 0 failed, 2 ignored**.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed.
- Explicit ignored tests against the release: **2 passed**, archive round trip
  and signal/workspace cleanup; no host package installation.
- Python verifier syntax and `git diff --check`: passed.
- Release atomically synced to the installed binary with byte equality; pin
  symlink retained. SHA-256:
  `0ad5013ea1a09651735edadd58a98b8b401cf544486dbb486c2290e083f68f57`.

Used the user-selected disposable Garuda/VirtualBox live-overlay guest, regular
guest execution over SSH, and actual Kitty/Xorg screen/scrollback capture.
The guest release matched the host checksum. Pacman/makepkg 7.1.0, libalpm
16.0.1, Kitty 0.48.2 and classic sudo 1.9.17p2 match the preceding verification.
Private VM identity/access details and raw evidence remain ignored/local.

**Six real-terminal scenarios passed:**

1. Separate controlled Flatpak/Nix operations and custom action review: active
   Cache tab, completed Updating tick, manager-scoped cache details, no duplicated
   pending cleanup rows.
2. Cache-disabling arguments without a cleanup command: no invented cache phase.
3. Real native/Sources upgrades, automatic pure-orphan removal and native cache
   cleanup while resizing: both update versions verified, retained optional
   orphans, Updating/Orphans ticks, active Cache and native cleanup details.
4. Redirected Sources dry-run: no terminal escapes or package mutation.
5. Password-required native/Sources/orphans/cache run: one visible prompt reused
   through the invocation, terminal restored and native cache actually cleaned.
6. Fresh subsequent invocation with an existing terminal ticket: exactly one new
   visible prompt and successful complete lifecycle.

The existing guarded verifier was run with `--section extras`, followed by the
sequence verifier's `--section auth-fresh`. Native/Sources used real
pacman/makepkg. Flatpak/Nix were controlled adapters, not real app upgrades.
A regular guest-only forwarding script pauses orphan/cache cleanup for one
second before execing original sudo/pacman, so fast empty-cache operations can
be sampled deterministically. No privileged helper binary was installed.

Actual rendered frames confirmed the example above and no completed package
rows during Cache; no-work/cache-avoidance cases omitted cleanup. Unit coverage
also checks incomplete/failed updates cannot earn a tick, active narrow tabs,
unchanged AUR pending rows, skipped cleanup diagnostics and external cache scopes.
Real AUR compilation, long ticket renewal and sudo-rs were not repeated here.

Evidence is local in the ignored `docs/verification/phase-tabs-evidence/` folder.
Guest pacman configuration and temporary password policy were restored, and both
fixture database locks were absent before shutdown. The guest shut down cleanly;
VirtualBox confirmed `VMState="poweroff"`. No host package was installed, removed or upgraded.
No GitHub publication or tag changes are authorized for this task.
