# Compact upgrade review and 3.3.0-alpha.1

## Changes

- Bumped Cargo package/lock version from 3.2.0-alpha.1 to 3.3.0-alpha.1. README
  now states that no stable release has been published and bugs are possible,
  without naming an older version as the current stability warning.
- Routine approval uses short unnumbered action labels. Detailed commands,
  internal Sources operations and preparation policies remain in dry runs.
  Typed action validation still runs before approval. Custom manager option
  arguments/commands and configured hooks retain their explicit review.
  Restart advice is not an approval-plan action.
- Installed-package space change is labeled separately from current Pacman
  cache bytes. Native no-cache plans may inspect configured cache roots without
  privilege, recursion or symlink traversal, with entry/time budgets. Incomplete
  scans are Unknown. Logical bytes are approximate and are not subtracted from
  package delta or peak installation need; snapshots/shared extents can prevent
  reclamation. Other managers' cache sizes are not guessed.
- A horizontal phase strip reuses the existing footer separator row. Detailed
  manager rows keep their names, metrics, pending/success/failure state and
  scrolling behavior. Selected orphan removal and requested actual cache
  cleanup have scoped pending lifetimes. Failure does not prematurely mark
  successful-manager cleanup as not run.
- Cache-disabling arguments are distinct from a post-upgrade cleanup command.
  Avoiding cache writes no longer fabricates a cleanup stage or success notice.
- Source lookup/approval and actual package-manager ordering remain with their
  existing owners. No next-restart/offline-update command exists in the current
  code or the retained main-history search; needrestart is advisory inspection.

AGENTS.md, README, architecture and transaction/UI contracts were updated.
There are no new dependencies, user flags or configuration files.

## Independent verification

- `cargo test`: **403 passed, 0 failed, 2 ignored**.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed.
- `PACPIN_TEST_BINARY="$PWD/target/release/pacpin" cargo test -- --ignored`:
  **2 passed**, archive round trip and signal/workspace cleanup. No host package
  installation was performed.
- Python verifier syntax and `git diff --check`: passed.
- Installed binary atomically replaced, byte equality checked, pin symlink
  preserved; installed version is **3.3.0-alpha.1**.
- Final release SHA-256:
  `a48b0b13b4d45118001d7f72c2daf722674858c9783cda9e5476f5751aa95c82`.

Unit coverage includes compact/default versus detailed dry-run review,
conditional/scoped labels, invalid approved actions, cache-estimate isolation
and unknown scans, symlink/nested-tree handling, bounded phase-strip width,
failure/pending cleanup state, custom option effects and avoiding phantom cache
cleanup for argument-only cache avoidance.

## Disposable VM verification

Used the Garuda live VirtualBox guest selected by the user, regular guest
execution over SSH and actual Kitty/Xorg screen/scrollback captures. The root
was a disposable overlay. Versions: VirtualBox host 7.2.22, Pacman/makepkg 7.1.0,
libalpm 16.0.1, Paru 2.1.0, Kitty 0.48.2, classic sudo 1.9.17p2. Private identity,
access details and raw evidence remain ignored/local.

| Suite | Passing cases | Coverage |
| --- | ---: | --- |
| Execution review | 9 | Decline, dry run, narrow native/Sources with one-Enter orphan selection, Sources only, noconfirm without autoremove, separate Flatpak/Nix, capability skip with keep-going, no work |
| Preparation/footer core | 16 | Scoped preparation/cache, retry, unsupported/no-work, native/Sources, live resize, interruption, ordinary native install, native download failure, verified AUR-failure continuation |
| Real AUR | 4 | AUR only, AUR/Sources, native/AUR/Sources, ordinary AUR install; actual Paru/makepkg/upstream hello, intermediate scoped transfer percentages and pending/completed rows |
| Final compact extras | 4 | Custom action review/execution, no phantom cleanup for cache-disabling arguments, native/Sources/cache live resize, redirected dry-run output without terminal escapes |
| Final authentication | 2 | Visible single password per invocation through prefetch/install/Sources/orphans/cache, fresh prompt on the following invocation despite an existing terminal ticket |

The broad 29-case matrices ran candidate
`b4b762c1e1c4ef76c94088a869ecc71036c1387c5f35e9771c49dc4e4da84edc`.
The final six cases ran the final SHA above, after the configured-adapter review
and cache-avoidance reporting refinements. Both copied binaries were checked
against their respective host releases. External Flatpak/Nix operations used
controlled adapters, not real application upgrades. Native/Sources and the AUR
suite used real guest package transactions.

The first narrow-table assertion incorrectly required `+ unknown` on one row;
it now accounts for wrapped table borders. A screen-sampling assertion also
missed an empty cache's very fast cleanup. The final extra fixture uses a regular
guest-only sudo-forwarding script that emits a test marker and pauses one second
for -Scc, then execs the original sudo and real pacman. It has no elevated file
permissions. This deterministically exposes the actual cache footer without
altering production timing. Both complete suites were rerun into fresh evidence
directories; initial failed-verifier evidence was retained, not counted as a
passing run.

Observed actual rendered review/prompts, horizontal phase labels, per-manager
progress and retained output at normal/narrow sizes. Password screenshots showed
the prompt before input; each final invocation reported exactly one password
prompt. Terminal settings and temporary-workspace cleanup checks passed.
The saved verifiers are `verify_execution_plan_vm.py` (default and `--section
extras`), `verify_preparation_vm.py --section core|aur`, and
`verify_upgrade_sequences_vm.py --section auth-fresh`, with their shared scripts.

Guest pacman configuration and the temporary password policy were restored;
the fixture database lock was absent. Evidence was exported before shutdown.
The guest shut down cleanly; VirtualBox confirmed `VMState="poweroff"`.

This does not establish real Flatpak/Nix/Pipx application upgrades, sudo-rs
authentication, a long ticket-renewal test, or a source compilation engine audit.
No host package was installed, removed or upgraded. Changes are intended for a
local commit only; no GitHub push or tag changes are authorized for this task.
