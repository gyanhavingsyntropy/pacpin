# Upstream compilation and resource cleanup verification — 2026-10-15

## Implemented contract

Sources supports explicit local `format = "source"` TOML recipes with literal
build argv, a relative staging output, declared build headroom/tools and optional
optimization settings. Git inputs resolve a release tag to an exact commit
before approval. Verified source archives retain the existing SHA-256 contract.
Compilation runs unprivileged through tracked makepkg execution; elevated
pacman receives only the successfully built and inspected package.

The default inherits makepkg configuration without adding CPU/optimization/LTO
flags. Native CPU, level and LTO are separate opt-in settings. Build systems can
override or ignore them; no performance improvement or universal compile-time
percentage is claimed. Commands are trusted user code, not a security sandbox.
See [the source build manual](../source-builds.md) for schema, examples and limits.

Initial real VM runs found two bugs: `makepkg --repackage` reruns `package()`,
and failed makepkg builds can leave mode-111 directories that ordinary cleanup
cannot remove. Packaging now uses a separately staged, inspected payload and a
packaging-only PKGBUILD, so compilation runs once. Shared owned-tree cleanup
pins directory descriptors before restoring access, refuses unowned directories
and filesystem boundaries, and leaves unrelated symlink targets untouched.
Initial failure evidence remains in `source-build-evidence/first.tgz` and
`second.tgz`; final runs below establish the corrected behavior.

## Local verification and binary identity

- `cargo test`: 341 passed, 0 failed, 2 ignored.
- Strict all-target Clippy and release build passed.
- Both ignored real-makepkg/signal tests explicitly passed with
  `PACPIN_TEST_BINARY` pointing at the release.
- Release, installed executable and guest executable SHA-256:
  `ee56cb7c35648eec584ce35f00c9dfdda16492d15ce9544d3dfcd24952fb94de`.
- Required logs and the separately completed AUR pinning checks are retained in
  [the AUR verification report](2026-10-15-aur-pinning.md).
- The final cached verification rerun also passed, with identical installed
  bytes; its logs are in `source-build-evidence/final-local/`.

## Disposable environment and reproduction

Garuda live ISO in VirtualBox, 3584 MiB and four CPUs, regular `garuda` user;
real Kitty screen/scrollback, real makepkg/compiler and pacman transactions.
Each suite serializes access to the guest terminal/config, owns an isolated
ALPM database/cache, and restores guest pacman.conf in `finally`.

The older ISO's waybar dependency prevented installing current jsoncpp.
CMake/jsoncpp/rhash/cppdap were downloaded and signature-checked with pacman
download-only, then extracted into a private guest toolchain directory. A
wrapper supplies only that CMake's library path. No desktop packages were
replaced for this setup. `prepare-source-vm.sh` records the guest-only steps.
This setup issue was outside pacpin's transaction path.

After copying the release, verifier scripts and exporter into guest `/tmp`:

```sh
PACPIN_DISPOSABLE_VM=1 bash /tmp/prepare-source-vm.sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_execution_boundaries_vm.py /tmp/pacpin \
  --output /tmp/pacpin-source-cleanup-final-20261015
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --section auth --aur --output /tmp/pacpin-source-auth-final-20261015
```

## Final VM results

### Source compilation: 13 passed

| Case | Result |
| --- | --- |
| Default archive build | Inherited `-O0`, no added native flags; executable reports unoptimized |
| Native plus compiled Sources, resize | Both update successfully; opt-in native CPU/level reaches compiler |
| Explicit LTO | `-flto` reaches the compiler; executable runs |
| Forced storage | Build and installation succeed with narrow terminal |
| Forced RAM | Explicit preflight refusal preserves the 2.5 GiB OS reserve; installed package unchanged |
| Compiler failure | No source installation and no leaked workspace |
| Missing staged output | No source installation and no leaked workspace |
| Missing build tool | Preflight stops before native/source changes |
| Missing build dependency | Preflight stops before native/source changes |
| Release tag moved after approval | Original approved Git commit and executable retained |
| Ctrl-C during build | Exit 130, installed package unchanged, workspace removed |
| Escaped executable symlink | Rejected before chmod; unrelated sentinel contents/mode preserved |
| Real GitHub cmark build | Compiled, packaged, installed and executed successfully |

cmark installed as `0.31.2-1`, from approved commit
`eec0eeba6d31189fd828314576494566d539b1e3`, with requested native CPU,
optimization level 2 and LTO disabled. The receipt binds that input and recipe.

A first final run exposed a **verifier assertion error**: footer cursor sequences
preceded the unique compiler marker on raw PTY lines. Four exact-line matches
therefore counted zero even though the marker occurred once. The strengthened
verifier now checks an installed counter file written by the actual build and
the unique log marker; each successful tiny compilation records exactly one
execution. The complete 13-case rerun passed. Initial transcript evidence is
retained separately; this did not require another product-code change.

### Shared cleanup/progress: 4 passed

Handled source cancellation removes registered workspaces; the next mixed
native/prebuilt-source transaction succeeds and completes native cache cleanup.
Foreground and orphan-review background downloads show aggregate percentages
for unequal archives. Installation progress shows the aggregate 50% at one of
two packages rather than a completed package's individual 100%. Orphan review
confirms on one Enter while remaining downloads stay visible.

### Authentication and real AUR combinations: 11 passed

Native-only, prebuilt Sources-only, mixed native/Sources, cache cleanup, real
AUR-only, AUR/Sources, all three managers, ordinary AUR installation and AUR
cache cleanup pass. Each actual invocation prompts once. Two consecutive
invocations in the same terminal prompt once each. The 100-second hook outlasts
the shortened ticket lifetime while noninteractive renewal succeeds; later
Sources installation does not request a second password. An injected failure
to start a fresh sudo session stops before changing installed packages.

Checks inspect rendered prompts before typing, resize while waiting, verify
terminal restoration/manager scope, and preserve native cache markers during
AUR-only cleanup. Real Paru builds GNU hello; it is not replaced by a fixture.

### Final guest state

All three suites restore the original pacman.conf. Temporary authentication
policy is absent; default and fixture ALPM locks are clear. Evidence was copied
to the host before ACPI shutdown. VirtualBox reports `VMState="poweroff"` and
`list runningvms` is empty.

These **28 new checks**, plus the **7 earlier AUR pinning checks on the same
binary**, establish **35 passing VM scenarios**. Deliberate transaction failures
and the forced-RAM refusal count as successful safety checks, not upgrades.

### Retained evidence

- `source-build-evidence/final.tgz`: 13 passing source-build cases, installed
  build counters, cmark provenance, rendered screens, transcripts and environment.
- `source-build-evidence/counter-transcript.tgz`: initial terminal-dependent
  assertion failures, preserved to explain the verifier correction.
- `source-build-evidence/cleanup-final.tgz`: four shared cleanup/progress cases.
- `source-build-evidence/auth-final.tgz`: eleven authentication and real AUR cases.
- `source-build-evidence/cmark-final.png`: actual guest display after cmark install.
- `source-build-evidence/prepare-source-vm.sh`: guarded private test-toolchain setup.
- `source-build-evidence/guest-final.log` and `vm-shutdown.log`: configuration,
  lock, executable identity and final poweroff evidence.

## Verification limits

Compilation checks exercise real C/Make and a real GitHub CMake project, cmark.
Rust optimization flag generation has unit coverage; a large Rust/Codex build
and application-specific PGO/BOLT are not exercised. The forced-RAM case may
accept only an explicit capacity refusal when the guest cannot maintain the
2.5 GiB OS reserve; this must be reported separately from successful RAM builds.
No host package install, removal or upgrade is performed. The private test
toolchain and fixtures live only in the disposable guest.
