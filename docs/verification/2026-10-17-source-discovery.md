# Independent Sources discovery — 2026-10-17

## Reported behavior and cause

The source-download wait notice exposed the worker implementation even though
downloads were visible in the shared footer. More substantially, upgrade checks
started only after all metadata refreshes, including Flatpak's. Source packages
already checked in batches of four, but one slow request held up the next batch;
any package error discarded every successful Sources plan.

## Changes

- Each integration owns an independent refresh-then-check pipeline. Sources need
  no refresh and start alongside native/Flatpak refresh. Native/AUR resolution
  still follows native metadata refresh. A failed refresh never checks stale
  metadata; native refresh failures remain fatal and finish bounded workers.
- The provider contract requires a lossless `discovery::Report`, allowing partial
  outcomes to survive aggregation. `Manager::Source(alias/package)` keeps an
  exact failed package distinct from a failure of the whole Sources adapter.
- `sources/discovery.rs` owns a rolling queue of at most four workers. Finished
  workers immediately take another package, with deterministic result ordering.
  Per-package errors and panics retain successful siblings, including those
  sharing an alias. Empty successful checks remain distinct from failures.
- The shared eligibility plan excludes exact failed source targets under
  `--keep-going` before approval, prefetch or execution. Healthy source plans
  remain approved snapshots. Strict runs and `pin check` require completeness.
  Unknown counts, incomplete/nonzero status, pending orphan cleanup and verified
  hook rules remain intact. Runtime dependency/capacity/install errors retain
  their separate safety rules.
- The footer warns about failed package checks alongside healthy Sources work;
  it does not mark the entire adapter skipped. User messages now say “Waiting
  for source downloads to finish” and omit the `[Background]` label.

## Local release verification

- `cargo test`: 365 passed, 0 failed, 2 ignored.
- Explicit ignored tests with `PACPIN_TEST_BINARY`: 2 passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed.
- Release and `$HOME/.local/bin/pacpin` verified byte-identical;
  `pin -> pacpin` preserved.
- SHA-256: `ccd18ccca0302243288578388f91a7b419276b1957d90b506a900ef9fb2ae02d`.

Unit tests cover manager-check overlap, a manager refreshing while another checks,
per-package overlap, a slow first package with later queued packages, bounded
concurrency, stable ordering, a failed/panicked source with successful siblings,
partial reports surviving manager aggregation, exact eligibility and footer state.

## Disposable Garuda verification

Final scenario results and rendered evidence are recorded alongside this report.
The source suite uses four installed packages across two aliases. One package's
catalog has no matching asset. Catalog reads are delayed and timestamped by a
small guest-only interposer to prove overlap rather than infer it from thread
creation. Native/Sources transactions use actual pacman/makepkg; Flatpak is a
controlled command adapter, not a real Flatpak application upgrade.

**27 selected scenarios passed:** 7 Sources cases and 20 shared manager-discovery
regressions on the exact release SHA above. The six unaffected cases from
`pacpin-source-discovery-vm-3` plus the rerun refresh case from `vm-4` form the
Sources set. Manager sets are `regression-1` (13) and `regression-3` (7).
The evidence summary records the superseded timing assertion explicitly.

Accepted verification covers strict, `--keep-going`, Sources-only, RAM/no-cache
dry-run, no remaining eligible work, read-only check and metadata refresh overlap.
Assertions include installed versions, failed target left unchanged, native
history retained through later Sources output, partial footer state, terminal
restoration, cleanup, and successful external upgrade/cache calls.

The initial fixture expected a forced-RAM installation despite the guest having
about 2399 MiB available, below the mandatory 2.5 GiB reserve. Pacpin correctly
refused it; that run is not counted as a successful installation test. Scoped
actual installs use automatic placement; RAM flags are covered in dry-run and
native discovery regression cases. A fresh-fixture rerun also exposed leftover
test `/opt/pacpin-ui-*` trees; the verifier now removes only its three known guest
app trees and executable links before/after use. No host package transaction was
used. Public GitHub publishing state and successful forced-RAM source installation
are not established by these fixtures.

Two evidence assertions were corrected without changing production code: source
reads may finish before the refresh helper starts its delay, so independence is
established by starts before refresh completion, plus the concurrent footer;
and the workspace checker must match the actual six-character mkdtemp suffix,
not mistake an in-test screen-capture text file for a leaked workspace. Affected
scenarios were rerun; superseded assertions are excluded from accepted totals.

## Final state

Evidence: summary and selected scenarios (local verification artifact).
Rendered frames and scrollback are retained in the per-run subdirectories.
The live screenshot shows the skipped-source-check warning alongside real
Pacman download progress, rather than a completed Sources transaction.
The guest's pacman configuration and hosts file were restored, with no ALPM
locks or source workspaces remaining. The confirmed VM shutdown state is saved
alongside the summary. No commit, push or tag change is part of this verification.
