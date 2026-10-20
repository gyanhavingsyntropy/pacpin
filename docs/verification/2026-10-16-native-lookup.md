# Native target lookup audit — 2026-10-16

## Reproduction and cause

The host command `pin -Syu --no-cache --use-ram --keep-going --dry-run`
failed before approval with `Transaction preview failed: conflicting dependencies`.
A lock-free ALPM diagnostic reproduced the exact pair: the bare `firefox`
target selected `cachyos-v3/firefox-pure` (157.0-1), conflicting with installed
`firefox` (157.0.1-1.1). The resolver had selected the exact `firefox` package
from a later repository. Preparing the same repository-qualified targets succeeded.
No host package was installed, removed or upgraded during diagnosis.

Preview searched an exact name and then providers within each database, allowing
an earlier provider to win before a later exact name was checked. Native try/run
archive selection and missing-dependency traversal had the same pattern.

## Changes and audited consumers

- `db::find_sync_package` now owns repository target selection. ALPM searches exact
  names across eligible databases before providers, retains dependency versions,
  and confines qualified targets to their repository.
- Preview, native try/run archives and missing dependencies, install routing,
  conflict checks and native stability-delay lookup use that boundary.
- Delay checks preserve full dependency expressions rather than dropping versions
  before deciding whether installed packages satisfy them.
- Preview reports conflicting package names/reasons and missing dependencies.
- Replacement removals match the full `replaces` constraint, including its version.
- Install preflight propagates its `needed` policy: forced same-version reinstalls
  remain in the transaction and capacity estimate.
- Ordinary native install dry-runs now validate the same prepared transaction
  and display direct targets, solver dependencies and removals. Delegated full
  system operations explicitly state that their dependencies are not expanded.
- A regression verifies that three external-manager check workers overlap.
  Existing scheduling already launches external checks concurrently with native
  resolution and threaded AUR lookup; installation ordering was not changed.

Reviewed resolver, requested upgrade planning, install, check, manifest rebuild,
source dependency/continuation checks, installed inventory and provider identities.
Resolver pin/vendor/exclusion policy, manifest classification and installed
inventory intentionally require exact identities. They must not replace an
installed package with a provider merely to consolidate code. Flatpak/Nix/Sources
keep their own typed identities and adapters; native ALPM lookup is not their API.

## Verification

- `cargo test`: 352 passed, zero failed, two ignored.
- Explicit ignored makepkg and signal-cleanup tests: two passed against the release.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed.
- Original host upgrade flags plus `--dry-run`: passed; correct Firefox target.
- Host `run --dry-run --use-ram firefox`: selected the exact Firefox archive,
  leaving installed files unchanged.
- Disposable Garuda VM: 15 actual CLI checks against small private ALPM fixtures,
  including native upgrade with/without the reported flags, check, install,
  try/run, repository-scoped providers, virtual targets, readable conflict errors,
  nonmatching replacement versions and a real Kitty terminal upgrade preview.
- The VM had insufficient RAM after its required reserve: that case verifies
  refusal, not successful RAM execution. Storage-based try/run previews verify
  exact target and dependency URLs.
- Guest pacman.conf was restored after each verifier run, including initial
  fixture assertion failures. No fixture packages were installed.

The initial verifier expected dependencies in the old command-only install
dry-run; this exposed the validation gap corrected above. A later run correctly
hit the VM RAM reserve and was adjusted to distinguish capacity refusal from
package selection. Only the final passing results are selected as evidence.

These are focused lookup/planning checks, not a complete security audit or an
assertion that every module has no bugs. Actual AUR, Flatpak/Nix installations,
authentication and extraction/commit recovery were not exercised in this audit.
The earlier transaction/UI reports retain their own separately scoped evidence.

## VM relocation

The powered-off VM directory was about 1.2 MiB and was moved using VirtualBox's
`movevm` to `/path/to/test-vm`.
Registered configuration and virtual disk paths were checked. The separate
3.1 GiB Garuda ISO remains on the VM storage server under the user's 200 MB relocation limit, so
booting the live guest still requires the VM storage server. Initial missing-device output was
sandbox visibility; the host VirtualBox devices were present.

Evidence is in `native-lookup-evidence/`; the reusable guest-only verifier is
`scripts/verify_native_lookup_vm.py`. The VM is shut down after evidence export.
