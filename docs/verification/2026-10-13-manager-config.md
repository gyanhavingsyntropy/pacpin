# Manager configuration and temporary execution — 2026-10-13

## Changes

- User-editable managers.toml, sources.toml and hooks.toml live beside config.toml.
  Explicit initialization preserves existing files; read-only commands do not
  create them. Legacy inline tables work; duplicate names are rejected. Automatic
  pin changes preserve supplemental definitions and manager-owned options.
- Literal manager commands, bounded JSON discovery and verification, scoped
  upgrade templates, explicit scratch/cache capabilities and namespaced flags.
  Standalone manager actions preview and require approval separately.
- Flatpak preserves installation/full ref and resolves abbreviated remote
  commits before approval. Nix checks and upgrades modern named flake profiles;
  Pipx constrains and verifies only approved Python environments. Failed discovery
  cannot produce a successful empty check. External-only refresh is scoped.
  No-work output describes the checked scope and omits absent native counts.
- Pipx uses PEP 440 rather than Arch's version ordering, including normalized
  equality, prereleases, epochs, post releases and local-version handling.
- Metadata commands are bounded, tracked, cancelled with their process groups,
  and refuse privileged configured-manager execution. Source HTTP agents and
  native configuration snapshots are reused; recent journal reads are bounded.
- Native try/run reports an existing version and can use package-owned installed
  binaries when no repository candidate exists, or explicitly with --installed.
  Repository versions can still be tested without replacing the installed version.
- Native repository run/try supports forced RAM/disk placement, no-cache archive
  bypass and dry-run preview. Downloads and extraction share one private workspace;
  live space/inode/RAM checks retain the 2.5 GiB reserve. Actual archive paths are
  counted before extraction; large payload bytes no longer imply millions of inodes.
- Preparation uses the shared App footer with aggregate bytes/percentage and short
  capacity/extraction phases. It retires before application execution. Tiny fixture
  downloads use KiB so their changing progress remains visible.
- Entry-point selection uses the requested package's inventory, including JDK-style
  toolkit directories, and refuses downloaded dependencies' executables.
- Scratch placement does not redirect application output. Host execution preserves
  ordinary writes; native try discards its private home/tmp. Only eligible project
  mounts support persistent writes through --rw; home-tree/system paths are excluded.
- Pipx run --no-cache owns its temporary environment, shared libraries and pip/UV
  caches and removes them on exit. Flatpak/Nix cache overrides and all external
  execution scratch overrides are refused when the adapter cannot guarantee them.

The real Flatpak check caught its short remote-ls commit format. The fixture
initially returned a full hash and did not establish this compatibility. Discovery
now obtains remote-info's full hash and checks it against the abbreviation.
See [upstream remote-ls implementation](https://github.com/flatpak/flatpak/blob/main/app/flatpak-builtins-remote-ls.c).

The real upgrade also exposed cursor-position queries from Flatpak's terminal UI:
their replies reached the shell input and broke the next command. The adapter
now selects its documented plain-output mode, preserving messages while pacpin
owns the footer. See [Flatpak environment reference](https://docs.flatpak.org/en/latest/flatpak-command-reference.html#environment).

## Verification

Final release SHA-256: `47064282c54a72b9b0a78148e7328aec3273283d78d73c22a15d03ed729d7151`.
The installed host binary matches byte-for-byte and the `pin` symlink is preserved.
`cargo test`: 312 passed, 2 ignored; explicit ignored execution: 2 passed.
Strict all-target clippy and release build succeeded. Cargo audit checked 163
locked dependencies against 1,310 advisories at database revision
`6aed84668c23d7f535077a9fe918bf1fa0f6a5bc`: zero advisories and warnings.

The final host RAM smoke test downloaded and ran tiny `core/which` in `/tmp`
without installation and removed its workspace. JDK dry-run accepted its 1651.6
MiB estimate without the former false inode requirement. Actual JDK execution
was not tested with a large download. The guest's 1536 MiB allocation deliberately
fails the production 2.5 GiB OS reserve; positive RAM execution evidence is the
host test. Guest storage tests establish non-tmpfs path selection, not physical
disk backing of the live overlay. Saved-output tests use tiny installed fixtures.

Real 44-column App footer with aggregate KiB and percentage (local verification artifact)

Rust and VM results are recorded in manager-config-evidence. VM suites run
sequentially in Garuda's disposable live overlay using real Kitty screens and
scrollback. Native/Sources transactions use real pacman/makepkg. Controlled
external-command fixtures are separate from real Flatpak/Nix/Pipx applications.

| Evidence archive | Passing checks | Release hash prefix |
| --- | ---: | --- |
| pacpin-installed-final-7.tgz | 34 | de44584f |
| pacpin-pipx-run-final-3.tgz | 5 | de44584f |
| pacpin-security-final.tgz | 4 | de44584f |
| pacpin-managers-config-final-4.tgz | 27 | de44584f |
| pacpin-resize-direct-final-2.tgz | 6 | de44584f |
| pacpin-upgrade-remaining-final.tgz | 15 | de44584f |
| pacpin-upgrade-audit-final.tgz | 19 | de44584f |
| pacpin-resize-direct-final-3.tgz (strict hook/history cleanup) | 6 | 47064282 |
| pacpin-upgrade-remaining-final-2.tgz | 15 | 47064282 |
| pacpin-upgrade-audit-final-2.tgz | 19 | 47064282 |
| pacpin-auth-final-3.tgz (including renewal and resize) | 11 | 47064282 |
| pacpin-progress-final-2.tgz (aggregate native percentages) | 2 | 47064282 |
| pacpin-real-final-11.tgz (real Flatpak/Nix/Pipx upgrades) | 9 | 017f0976 |

Each archive has results, rendered screen frames, raw output and environment
metadata. Earlier archives are diagnostic or intermediate runs, not additional
passing checks of the final release. Installed-final-6 completed its 34 assertions
but failed in fixture directory cleanup; final-7 fixes and reruns that cleanup.
Pipx-run-final timed out during a network bootstrap, and final-2 stopped before
the run cases because the guest system Python has no pip module. Final-3 uses
Pipx's own pip interpreter and offline wheels for those execution checks.
The October 13 full-suite run, upgrade-final-4, recorded a native hook screen
failure during resize and stopped at a Pipx screen assertion:
raw output contained the footer but the one-second fixture phase was missed by
the screen sampler. The October 14 continuation uses three-second Nix/Pipx phases
and retains the original assertion; both provider footer captures then passed.
Native hook fixtures also retain their phase longer for reliable screen sampling.
The auth-final-2 renewal case subsequently reproduced real leftover footer text
in native history despite successful one-prompt authentication. The resize code
now clears the old footer within the visible viewport before reserving its new
region; suspension and teardown use the same coordinates. Direct resize checks
also reject leftover text in native hook lines. Pre-fix archives remain distinct.

The final `47064282` release passes all 53 checks across direct resize, remaining
upgrades, failure/cleanup audit, authentication and aggregate progress suites.
The renewal test preserves every native history line and asks once; consecutive
invocations ask once each. Configuration/sandbox and real-manager archives from
earlier hashes establish those unchanged paths before the isolated footer fix.
The final host binary again passed the RAM utility smoke test and JDK preview.
The App screenshot and configuration timing samples precede the footer fix.

Reproduction commands, run inside the prepared guest:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_managers_vm.py /tmp/pacpin --output /tmp/manager-config-results
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_installed_run_vm.py /tmp/pacpin --output /tmp/installed-run-results
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_real_managers_vm.py /tmp/pacpin --output /tmp/real-manager-results
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin --aur --output /tmp/upgrade-ui-results
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin --aur --section remaining --output /tmp/upgrade-remaining-results
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin --section audit --output /tmp/upgrade-audit-results
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin --aur --section auth --output /tmp/auth-results
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin --section progress --output /tmp/progress-results
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_security_vm.py /tmp/pacpin --output /tmp/security-results
```

The installed-run fixture tests host/sandbox execution, package-owned --bin,
toolkit executables, refusal of dependency entry points, output persistence,
missing packages, explicit repository conflicts, and a newer repository version
while the installed version remains unchanged. It also tests scratch/cache/preview
composition, narrow aggregate progress and workspace removal. Real Flatpak uses a tiny local
OSTree repo; Nix a local Git flake; Pipx a small PyPI application. The Nix fixture
temporarily permits its tiny local builder in the guest and restores daemon config.
Pipx's successful/failed ephemeral execution uses real wheels downloaded once and
installed offline, to separate cleanup behavior from transient bootstrap downloads.

Resize cases use Kitty's own tty and capture rendered screens/scrollback rather
than introducing script(1)'s extra PTY, whose delayed resize forwarding caused
one recorded stress case to draw old dimensions. Other cases retain raw script
recordings plus real Kitty frames. Repeated direct native/Sources resizes also
check native history for leftover footer text and normal wrapping after exit.

The VM uses 1536 MiB during this verification to preserve host RAM headroom.
Shutdown was confirmed on October 14 (`VMState="poweroff"`), and its original
3584 MiB setting was restored. A first real AUR build
exceeded the old 180-second harness timeout during debug indexing. The test
fixture now disables debug/LTO and permits 600 seconds; production build flags
are unchanged. Earlier failed diagnostic archives remain distinct from passing
final runs.

## Configuration overhead

Forty interleaved warm runs of config validate per condition, excluding five
warmups, with the VM running. Main config only: median 17.72 ms. Main plus shipped
manager template and empty optional source/hook tables: 17.83 ms. Difference:
about 0.11 ms. An earlier busy-VM measurement was 27.10/28.90 ms; that raw result
is retained separately. These timings are noisy and do not establish a hard
overhead bound. This measures CLI startup/validation, not complete upgrades,
uncached filesystem performance, networking or the execution of custom scripts.
The inputs, launcher and measurements are in manager-config-evidence.

## Limits

Configured scripts are trusted user code; verification is their explicit
contract, not independent understanding of an arbitrary manager's database.
Temporary RAM/storage selection does not move final stores or override daemon
build policy. Cleanup is explicit and successful-manager scoped; broad pruning
is not silently substituted for cache deletion. Pacman's ALPM/elevation and
AUR's unprivileged helper contracts remain typed rather than arbitrary templates.

General hook conditions still cover verified native/AUR/Sources version changes;
repository-only changes and external manager identities are not hook triggers.
External updates have separate verified history entries; native cache rollback
refuses them. Nix/Pipx/custom entries are not added to the package manifest.
Legacy search/install AUR wrappers and some native query/unknown-flag forwarding
still need a separate boundary migration. This is not an exhaustive audit of
system libraries, scripts, package hooks or every integration/backend version.
