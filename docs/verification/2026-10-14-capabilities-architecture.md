# Capability policy, execution ownership and manager footer — 2026-10-14

## Scope and behavior

Baseline: local commit `545782b`. This report follows the earlier
[module-layout verification](2026-10-14-module-layout.md); that report describes
the first split and its earlier binary, not the final behavior in this report.

Upgrade preflight now classifies options as supported, unsupported or invalid.
Every integration must implement the typed capability method. With --keep-going,
an explicitly unsupported option excludes only its manager before counts, hooks,
approval, prefetch and execution. The skipped manager performs no upgrade,
custom action or cache cleanup; the invocation remains incomplete/nonzero and
preserves pending orphan cleanup. No-work managers are not validated for irrelevant
options. Invalid configuration, unavailable executables, capacity failures and
uncertain metadata still stop the invocation. Runtime continuation retains its
separate verified AUR-build-failure-to-Sources contract.

Core AUR and external stages have distinct identities. An external manager named
AUR cannot accidentally exclude core AUR targets or conceal unsupported helper
options. Reserved built-in manager keys must retain their adapters; custom
display names cannot shadow Sources, Flatpak, Nix or Pipx.

The footer records approved stages before native downloads and orphan review.
AUR, Flatpak, Nix and other eligible managers remain Pending until actual work
starts. External metadata refresh/check workers publish separate scoped manager
rows to the single terminal renderer. Checking updates never earns an upgrade
tick. Actual source prefetch replaces its pending row without a duplicate; failed
transactions mark remaining pending work not run. Narrow terminals summarize
overflow and preserve space for ordinary output. Read-only check uses the same
discovery rows; quiet and redirected output do not allocate a terminal footer.

Example during a mixed upgrade:

```text
Pacman: Downloading packages (41%)
AUR: Pending
Sources: Queued; waiting for repository downloads
Flatpak: Pending
Nix: Pending
```

Rows represent only work in that invocation. External upgrades still run in
their existing order; pending rows do not imply concurrent downloads.

## Responsibility-based module layout

- main.rs is a 56-line entry point. CLI startup/dispatch, help and tests live
  under cli/. Pin matching and smart unpin belong to commands/pins.
- execution/ owns tracked children, graceful cancellation, bounded capture and
  registered resources. It imports neither sandbox nor progress; the terminal
  owner registers a restoration callback. Cleanup-registry tests use independent
  instances instead of draining resources belonging to parallel tests.
- sandbox.rs is a 130-line facade. Native preparation, provider applications,
  mounts, workspaces, executable selection and tests live under sandbox/.
- progress/ owns normalization, aggregate metrics, scoped stage state and tests.
  progress.rs retains the cohesive terminal/session and output transport.
- commands/upgrade/preflight owns one eligibility result; planning owns target
  classification. pm/ owns installation, cache, lock and validation operations.
- Shared package executable-path policy lives in utils for archive, database
  and sandbox inventory. No new runtime dependency or user configuration file
  was introduced.

[Architecture and change-impact map](../architecture.md) records actual callers
and remaining legacy execution paths. [Design contract](../development/design.md)
applies professional abstraction, ports/adapters and Rust API guidance. These
changes enforce specific boundaries and capability declarations; they do not
claim that a single binary crate forbids every incorrect dependency or that all
long orchestration functions have been eliminated.

Seven moved blocks preserve their bodies after normalizing owner paths,
entrypoint naming and necessary visibility. See
source movement evidence (local verification artifact). Typed
capability preflight, instance registry ownership and footer state are deliberate
behavior changes and are verified separately.

## Bugs found while doing this work

- Parallel cleanup-test interference existed before the overhaul. Changed test
  timing exposed it; instance-owned test registries now preserve other workspaces.
- The new capability planner initially used display names for AUR identity. This
  was introduced in the new implementation and fixed with a typed core/external
  stage distinction, two regression cases and VM identity checks.
- The previous all-manager validation stopped otherwise eligible upgrades on an
  unsupported external option, despite --keep-going. The new preflight changes
  that policy while retaining fatal invalid/uncertain failures.
- Missing pending AUR/external rows and generic external check status were
  pre-existing UI gaps; scoped activity and approved stage registration address
  them.

## Rust verification and binary sync

- cargo test, including a repeat: **326 passed, 0 failed, 2 ignored**.
- Both explicitly ignored tests passed: real makepkg archive round trip and CLI
  signal cleanup with PACPIN_TEST_BINARY set to the release.
- cargo clippy --all-targets -- -D warnings: passed without warnings.
- cargo build --release: passed.
- Installed binary atomically replaced at $HOME/.local/bin/pacpin; byte
  equality checked and pin symlink preserved. Host configuration validates.
- Final release SHA-256:
  `a737d015b9685584ccb3f17a9d3068fd7f7008b29b1b9a38f632f2b47218132f`.

Logs are retained in architecture-evidence (local verification artifact).

## Disposable VM verification

pacpin-test-vm runs the live Garuda overlay under VirtualBox with 3584 MiB,
four CPUs and Kitty on X11 display :0. CLI invocations run as garuda; actual
native/Source fixture transactions use pacman/makepkg with isolated guest
databases/cache. External commands and the dedicated pending-AUR failure check
are controlled fixtures, not real Flatpak/Nix upgrades or an actual AUR build.
The separate authentication suite uses real Paru/hello combinations.

| Final suite | Passed | Failed |
| --- | ---: | ---: |
| Pending AUR, separate Flatpak/Nix, resize, no-work, failure and redirected output | 7 | 0 |
| Read-only/quiet checks and concurrent external metadata refresh | 3 | 0 |
| Cancellation/resource cleanup and aggregate foreground/background native downloads | 4 | 0 |
| Typed capability preflight, continuation and scoped cleanup | 14 | 0 |
| Built-in identity/adapter validation | 2 | 0 |
| Real sudo prompts, renewal, native/Sources and real Paru combinations | 11 | 0 |
| Total | **41** | **0** |

All final suites used the exact release hash recorded above. The authentication
suite verifies one prompt within each invocation, fresh authentication on the next
run, a visible prompt after resize, and renewal through a deliberate 100-second
wait. Native progress checks use unequal real archives and verify whole-stage
percentages, including downloads during orphan review.

The guest pacman configuration matches its saved original, temporary sudoers
policies are absent, original and fixture database locks are absent, and the
serial verifier lock is released. Cleanup evidence is retained in
architecture-evidence/guest-cleanup.log. The VM is confirmed powered off with its
3584 MiB memory and four-CPU settings preserved; no host package transaction was
used for testing.

Reproduction inside the disposable guest:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_manager_footer_vm.py /tmp/pacpin \
  --output /tmp/pacpin-footer-verified
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_manager_footer_vm.py /tmp/pacpin \
  --section discovery --output /tmp/pacpin-discovery-verified
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_execution_boundaries_vm.py /tmp/pacpin \
  --output /tmp/pacpin-execution-verified
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_capabilities_vm.py /tmp/pacpin \
  --output /tmp/pacpin-capabilities-verified
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_capabilities_vm.py /tmp/pacpin \
  --section identities --output /tmp/pacpin-identities-verified
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --section auth --aur --output /tmp/pacpin-auth-verified
```

Suite output directories must be fresh; shared terminal/config ownership requires
sequential execution. Each archive is copied to the host immediately after its
suite, including rendered frames/scrollback, exit statuses, installed versions,
terminal settings, environment and binary hash.

Earlier broader post-overhaul suites completed on intermediate binaries, but an
unexpected shutdown destroyed their archives before host export. They are not
counted as retained final evidence. The first split's 73 checks remain preserved
separately in modularity-evidence. Two initial footer-verifier runs exposed
incorrect fixture expectations (external cache row identity, build-phase wording,
and reused external state in a redirected case); their failed results are
retained as footer-first.tgz and footer-second.tgz. No Rust changes were made
between those runs and the final corrected footer suite.
