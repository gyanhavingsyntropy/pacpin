# Scoped upgrades and conditional post-update hooks — 2026-10-11

## Changes

- Added upgrade-only `--sources-only` / `--aur-only` scopes. Scoped `-Syu`
  excludes native repository refresh and unrelated manager work.
- Added invocation-only `--use-ram` / `--use-storage` for Sources and Paru
  temporary workspaces. Forced media retain quota, inode and memory checks;
  RAM requires a 2.5 GiB OS reserve. These controls do not move installed files
  or remove root installation capacity requirements.
- Added opt-in `--keep-going` for independently validated Sources after a
  recognized Paru build failure. Native database/log changes, uncertain failure,
  native installation damage and interruption block continuation. An AUR failure
  remains a failed overall run and its cache is preserved.
- Added configured post-update hooks with `watch` (any), `watch_all` (all) and
  `unless_updated` (veto). Hooks run once after verified version changes in
  successful upgrade transactions, before orphan/cache cleanup and restart advice.
  Scripts execute as the regular user with literal arguments. AUR rebuilds use
  the shared Paru execution boundary and force same-version rebuilding.
- Added dependency verification before successful rebuild workspace cleanup,
  concise hook phases and correct completed-manager footer rows. Rebuild failure
  preserves its workspace and prevents later hooks from running.
- Serialized the VM verifiers because they share Kitty and guest pacman.conf.
  Concurrent suites are rejected before mutating guest configuration.

Configuration and composition examples are in [README.md](../../README.md#post-update-hooks).
No hook was activated in the user's host config and no host package was installed,
removed, rebuilt or upgraded by these checks. Recovery development remains deferred.

## Rust and installed binary

Executed with `PATH="$HOME/.cargo/bin:$PATH"`:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
cmp target/release/pacpin $HOME/.local/bin/pacpin
```

- Tests: **270 passed, 0 failed, 2 ignored**. The existing explicit-only
  `real_makepkg_archive_round_trip` and
  `cli_signals_remove_source_download_workspaces` tests were not separately run.
- Strict Clippy and release build passed.
- Release was atomically installed; byte equality was verified and `pin -> pacpin`
  was preserved.
- Final release/installed/guest SHA-256:
  `b667af377d3ada0f342f83ce28bdac49dcd12cfa2a39a7dab8d4a76918c73548`.
- All three verifier scripts passed Python AST parsing; `git diff --check` passed.

## Disposable Garuda verification

Guest: VirtualBox `pacpin-test-vm`, Garuda live overlay, regular `garuda`
account, real Kitty on SDDM Xorg display `:0`. Host memory constraints required
temporarily reducing the guest from 5120 to 3584 MiB. The guest had roughly
2497 MiB available at the forced-RAM check, so the requested workspace plus
2.5 GiB OS reserve could not fit.

Run these inside the guest, sequentially, after copying the scripts and binary:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --section audit --output /tmp/pacpin-audit-final
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --section auth --output /tmp/pacpin-auth-final
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_runtime_flags_vm.py /tmp/pacpin \
  --output /tmp/pacpin-runtime-verified
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_post_update_hooks_vm.py /tmp/pacpin \
  --output /tmp/pacpin-hooks-verified2
```

**55 cases passed**, with zero assertion failures. Expected nonzero CLI statuses
in injected failure cases are successful verifier results, not successful upgrades.
Machine-readable summaries are preserved in [runtime-hooks-results.json](evidence/runtime-hooks-results.json).

| Suite | Cases | Coverage |
| --- | ---: | --- |
| UI audit | 19 | Narrow restart advice, dependency counts/order, scoped cleanup, failed cleanup, storage refusal, orphan Enter during/after downloads, cancellation and lock release, actual injected native extraction failure |
| Authentication | 6 | Native-only, Sources-only, mixed transactions, consecutive runs, no-cache, ticket renewal during a long run, injected ticket-reset failure |
| Runtime flags | 16 | Sources/AUR scopes, custom storage path containing spaces, forced RAM refusal, conflicting/wrong-operation flags, normal stop versus guarded continuation, unknown/native/log-change/signal failure refusal |
| Conditional hooks | 14 | Dry-run preview, OR/AND/veto conditions, verified Source/native changes, no-op versions, recursion prevention, script failure, failed Source suppression, scope exclusion, actual same-version archive reinstall, failed post-rebuild dependency verification |

Artifact chronology: the audit/auth suites used release `7ce88e…`; the runtime
suite started on `3340…`. The final `b667af…` artifact was uploaded atomically
during runtime checks and used for the full final hook suite. Later Rust changes
were confined to AUR rebuild verification/cleanup. The audit/auth suites were not
rerun on the final hash.

Native and Sources fixtures use real pacman/makepkg with an isolated guest
database and small local packages. AUR failure/continuation tests use a controlled
helper named `paru`. The hook rebuild fixture uses that adapter with real makepkg
and pacman: a same-version rebuilt archive changes an installed file marker.
This is not evidence of an actual Antigravity build or a complete real-Paru remote
clone/build transaction. Other integration executables in the audit are controlled
fixtures, not actual application upgrades.

The forced-RAM case specifically passed by refusing insufficient reserve. No
successful forced-RAM transaction was demonstrated in this guest. Unit tests cover
policy selection and capacity boundaries. In a live overlay, a non-tmpfs filesystem
can itself be backed by RAM; forced storage selects filesystem type, not physical
hardware. Host dry runs confirmed Sources-only RAM and AUR-only storage plans,
including manager-specific no-cache previews, without installing packages.

### Rendered evidence and teardown

Inspected Kitty rendered frames and scrollback, including a 40-column native plus
Sources run, Sources-only packaging, orphan confirmation followed by download
waiting, and a verified AUR rebuild followed by a user script. Completed Pacman/AUR
or Pacman/Sources rows remain checked while `Hooks: Running user script` is active.
The final VirtualBox console screenshot was also visually inspected: it shows the
injected missing-dependency rebuild failure, retained workspace, skipped remaining
hooks, successful native cache cleanup and preserved failed AUR cache.

Detailed logs, frame captures, screens and screenshots are retained locally in
`target/maintenance/runtime-flags-20261011/` (ignored build evidence). Relevant
exports are under `evidence/pacpin-runtime-verified`,
`evidence/pacpin-hooks-verified2`, `evidence/pacpin-audit-final` and
`evidence/pacpin-auth-final` within that directory.

Initial fixture/assertion failures and an accidentally overlapping suite were
corrected before the successful runs. The runner now uses a shared lock and shell
chains preserve earlier failures. The incorrectly configured earlier guest plan
was refused by native capacity checks before installation. No host transaction
was performed.

After verification, guest pacman.conf matched its saved original, temporary sudo
authentication policy was absent, and the native pacman lock was absent. Evidence
was exported before shutdown. The VM is confirmed **poweroff**, and its original
**5120 MiB** memory setting has been restored.
