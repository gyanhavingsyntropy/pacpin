# Discovery continuation and eligibility — 2026-10-17

## Reproduced issue and upstream evidence

The user's `pin -Syu --no-cache --use-ram --keep-going` stopped on a Sources
update-check failure before upgrade eligibility was evaluated. Discovery returned
one `Result<Vec<Update>>`, so an error discarded successful managers' results.
The AUR resolver similarly exposed an untyped error list which upgrade treated
as globally fatal. This was a gap in the result contract left by the earlier
module refactor, not something a directory split or AGENTS.md rule prevented.

The original Sources error was reproduced without changing host packages:
`Expected exactly one asset ... found 0`. Public GitHub API metadata captured
during this investigation showed `rust-v0.164.0-alpha.2` with 36 assets and no
`codex-package-x86_64-unknown-linux-musl.tar.gz`; the preceding alpha.1 had 318
assets, including that exact filename. The newer list contained macOS/Windows
assets. This establishes an absent matching upstream artifact at that time,
not a confirmed permanent filename change. Incomplete/failed publishing is an
inference; no pipeline cause was proved. Configured recipes were not rewritten
and pacpin does not silently pick an older release.

Primary sources:

- https://api.github.com/repos/openai/codex/releases?per_page=2
- https://github.com/openai/codex/releases/tag/rust-v0.164.0-alpha.2
- https://github.com/openai/codex/releases/expanded_assets/rust-v0.164.0-alpha.1

The small filtered snapshot is saved alongside the verification evidence.
The source diagnostic now includes the selected release tag and asset pattern.

## Enforced boundaries

- `discovery.rs` owns a must-use, lossless report of successful and failed manager
  checks. Manager and phase identities are typed; core AUR is distinct from an
  external display name of AUR. Empty successful checks remain distinct from
  failures. Native transaction failures are not representable as a skippable
  native discovery manager.
- Integration refresh/check collectors return reports. Read-only `check`
  explicitly requires completeness. Upgrade must supply the recorded failures
  to the shared eligibility planner along with capability checks.
- A failed external refresh excludes that provider before any stale update check.
  A failed AUR lookup does not become a false missing-pin result. Existing explicit
  AUR requests are deferred when the AUR discovery already failed, rather than
  separately retried through request planning.
- Eligibility projects one approved inventory. Failed managers cannot enter
  upgrades, prefetch, namespaced actions or cleanup. Source ownership remains
  intact, so a failed Sources check cannot replace Codex with a native/AUR package.
- Unknown update counts are shown as unknown, not zero or up to date. With
  --keep-going, eligible managers proceed; skipped work keeps exit status 2,
  including no-work and dry-run cases. Pending orphan cleanup is preserved.
- Footer skips require a typed cause: unsupported options, update-check failure,
  or metadata-refresh failure. The first VM review exposed the previous hardcoded
  unsupported-options label; the final renderer and regression checks fix it.
- Hooks use verified successful changes and the same AUR eligibility decision.
  An unrelated Sources failure does not suppress a native-triggered script.
  Typed AUR rebuild hooks cannot bypass a skipped AUR stage. Actual failed
  transactions still stop hooks under their existing contract.
- Successful invoked managers may clean their own caches. Failed/skipped managers
  retain their caches. Native refresh/dependency/installation failures, invalid
  local configuration, invalid prepared plans, capacity refusal and unknown
  coordinator failures remain fatal. Existing guarded runtime AUR build-failure
  continuation is separate and was not widened.

## Verification

- Full Rust suite: 360 passed, zero failed, two ignored.
- Both explicit ignored source packaging/signal tests passed against the release.
- Strict all-target Clippy and release build passed.
- Host exact flags plus --dry-run continued the native plan, showed the failed
  Sources check/release/pattern, and returned incomplete status. Sources-only
  strict dry-run stopped with the full diagnostic. No host package transaction
  or host cache purge was performed.
- Disposable Garuda real-terminal scenarios use `scripts/verify_discovery_vm.py`
  with the shared capability/UI verifier. Selected acceptance evidence records
  each case's binary hash, real installed native/Source versions, rendered
  frames, scrollback, exit status, terminal restoration and resource checks.
- Final acceptance comprises 20 passing scenarios (13 discovery cases and seven
  guard/eligibility cases), all on release SHA-256
  `65386f2c56fa84eb8307fdfdf7495a61acfa44fca82aa1263c4c35d35e5019ff`.
- Native/Sources fixture transactions use real pacman/makepkg. Flatpak, Nix,
  Pipx and custom adapters are controlled command fixtures, not application
  upgrades. AUR RPC failure is injected through guest-only /etc/hosts; both
  hosts and pacman configuration are restored in finally.
- Coverage includes strict/keep-going, all-failed/no-work, exact flags, dry-run,
  all adapter families, refresh failure without stale recheck, AUR requests,
  scoped exclusion, multiple skips, typed footer causes, successful-manager
  cleanup, pending orphans, native dependency refusal and eligible hooks.
- Earlier review binaries are not selected as final acceptance. Discovery tests
  shorten the UI suite's artificial source read delay; they do not establish
  long-review/download timing or password-authentication coverage.

This enforces and tests discovery-to-eligibility handling. It is not a universal
transaction rollback mechanism or a claim that every future module is bug-free.
Manager availability detection and other legacy ownership exceptions retain
their separately documented interfaces. Fresh explicit target validation still
has its normal fatal-input contract.

## VM location and final state

The user requested reverting the small VM-directory relocation because its live
ISO still requires the VM storage server. After verification and clean shutdown, the registered
VM directory is moved back to `/path/to/test-vm`.
The separate Garuda ISO stays at its existing the VM storage server path. The VM access document
records the current location. Final-state evidence records poweroff, restored
guest configuration, and byte equality of the release/installed binary.
