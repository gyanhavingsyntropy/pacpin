# Orphan cleanup retry — 2026-10-11

## Report and cause

The user reported that a later upgrade did not offer orphan selection after an
AUR failure skipped removal. The existing `known_orphans.json` inventory already
contained these packages. `has_changed` checks new names and changes in whether
a package is a pure orphan; it does not represent unfinished cleanup. A read-only
orphan listing can also update that inventory. The previous assurance that failure
alone guarantees another prompt was incorrect.

## Changes

- Track confirmed cleanup separately in `pending_orphan_cleanup.json` under the
  pacpin state directory. Save atomically with a private temporary file, file
  synchronization, rename and directory synchronization before starting package
  installation/removal. A failed state write aborts before those operations.
- Re-offer currently eligible pending names on a later full upgrade, even when
  the inventory is unchanged or no updates remain. Stored names only request a
  new review; they do not authorize automatic removal or restore old selections.
- Keep failed/skipped cleanup pending. Successful removal clears completed names;
  names no longer orphaned are checked against the fresh package graph.
- An explicit new review with no selected packages acknowledges that unchanged
  list. Read-only listings, scoped upgrades and noninteractive upgrades without
  autoremove do not acknowledge unfinished cleanup.
- Introduce a one-time review for existing inventories without the new state,
  because older releases did not retain skipped selections.
- Apply the same completion handling to `pin orphans -c`, and return nonzero when
  removal or state completion fails on the no-update upgrade path.
- State ownership stays in `OrphanManager`; command handlers use its boundary API.
  Document the behavior in README and add a regression contract to AGENTS.md.

## Verification

With `PATH="$HOME/.cargo/bin:$PATH"`:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
cmp target/release/pacpin $HOME/.local/bin/pacpin
```

- **275 passed, 0 failed, 2 ignored**. The existing explicit-only ignored tests
  were not separately run.
- Strict Clippy and release build passed. Atomically synced the installed binary
  and verified byte equality; preserved `pin -> pacpin`.
- Final binary SHA-256:
  `97a8889d71a4eea438cf23ccc5c15e36c60ef4a779eb46cc0608aae451b00bf1`.
- Unit regressions cover unchanged known inventories with pending cleanup,
  migration, explicit decline, stale names, atomic persistence, preservation
  after failed removal, completion, invalid state and failed-write cleanup.
- Python AST parsing and `git diff --check` passed.
- Host read-only `pin -Su --dry-run --pkexec` offered the already-known 17-orphan
  inventory. No host package was installed, removed or upgraded. The host pending
  state was not created by that dry run.

### Disposable VM

Garuda live overlay in VirtualBox `pacpin-test-vm`, regular `garuda` account,
Kitty on Xorg display `:0`/VT2, temporarily 3584 MiB memory. Tested with the final
binary above; only isolated guest package state and small fixtures were modified.

Executed sequentially inside the guest:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_orphan_retry_vm.py /tmp/pacpin \
  --output /tmp/pacpin-orphan-retry-verified
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --section audit --output /tmp/pacpin-orphan-retry-audit
```

The retry suite uses real makepkg and pacman for a small dependency package.
An AUR helper named `paru` is a controlled build-failure fixture, not a real
Antigravity build. Removal failure is injected by a guest sudo wrapper for the
specific `pacman -Rns` operation. Native extraction failure in the separate UI
audit uses real pacman writing to a constrained fixture filesystem.

Retry cases include known inventory before AUR failure, pending preservation
across a read-only listing/noninteractive/scoped run, second-invocation removal
with no updates, completion without repeat prompts, failed removal/retry,
explicit decline without repeat prompts, legacy-state migration, a blocked state
write, and direct cleanup. Rendered Kitty screens/scrollback were inspected for
the 40-column AUR-failure and no-update retry selectors, removal retry and explicit
decline. These establish that a fresh selector appeared and the real package
was removed only after confirmation.

**31 cases passed, with zero failed assertions**: 12 orphan retry cases and 19
surrounding UI audit cases. Expected nonzero CLI statuses in injected failures
are successful verifier results, not successful package operations. Compact
results are preserved in [orphan-retry-results.json](evidence/orphan-retry-results.json).
The initial retry-runner attempt lacked output-directory creation; that setup
error was fixed before the successful fresh-directory run.

The audit also rechecked dependency-summary ordering, cache cleanup ordering and
failure status, storage refusal, native/Sources history retention, orphan Enter
during/after downloads, Ctrl-C and database lock release, injected actual native
extraction failure, and controlled integration failures. Integration executables
are fixtures, not evidence of actual application upgrades. A real VirtualBox
console screenshot was visually inspected after source installation, and the
rendered post-selection wait showed the native download footer before Ctrl-C.

Detailed logs, frame exports and screenshots are retained locally under
`target/maintenance/orphan-retry-20261011/`. Before shutdown, guest pacman.conf
matched its saved original, temporary authentication policy was absent, and
native plus both isolated database locks were absent. Evidence was exported.
The VM is confirmed **poweroff**, with its original **5120 MiB** allocation
restored.
