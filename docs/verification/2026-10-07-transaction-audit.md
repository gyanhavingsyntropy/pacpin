# Transaction and terminal audit — 7 October 2026

## Reported failures

The host's native upgrade exhausted writable root space while extracting
WebKit. Earlier packages had already changed despite pacman's final generic
“no packages were upgraded” message. The native cache was retained because
the upgrade cleanup path only ran after overall success. Source download
verification subsequently completed, which was not source installation.

The old plan card counted the requested updates before showing solver-added
dependencies. This omitted both new dependencies and additional upgrades
pulled into the native transaction. The requested 41 native updates plus one
Source were not the complete execution count: the logged native transaction
had 49 entries, including seven new dependencies and an extra installed
dependency upgrade.

## Changes

- Prepare the native transaction before displaying its final summary. Show
  dependencies, additional dependency upgrades and removals first. Count and
  size the prepared native plan, explicitly include new dependencies, and
  omit zero additions. Read-only checks use the same summary renderer.
- Check estimated native peak root/cache usage before downloading and again
  before installation. Check root capacity for direct source installation
  before building, and recheck against the actual built package. Explain why
  final net growth and temporary peak usage differ. `--no-cache` does not
  relocate pacman's installation destination or remove its peak writes.
- Report cleanup per invoked manager, independently of journal success or
  later manager failure. Clean successful managers, explicitly preserve
  failed-manager archives for recovery, and explain when there is nothing to
  clean. A failed requested cleanup makes the overall command fail without
  denying that installation succeeded. Cache confirmation preserves sync
  databases. Prune only stale empty ALPM download directories after lock checks.
- Remove selected orphans before cache cleanup, then print restart advice.
  A failed requested orphan removal returns failure. Report possible partial
  changes after native commit failure and explicitly skip later upgrades.
- Show short footer phases for authentication, confirmation, dependency
  resolution, download, verification, installation, packaging and hooks.
  Retain available native package counters and percentages without copying
  prompts, long package rows or arbitrary hook output into the footer.
- Record completion in the orchestration layer after actual upgrade success.
  A successful download or generic subprocess does not complete a manager.
  Keep checked completed rows while subsequent managers run.
- Display background repository downloads during orphan review through the
  shared footer. Keep its child output away from selector input and logs.
  Preserve the complete inventory in scrollback, provide a scrolling viewport
  with Home/End and page navigation, and confirm on one Enter during search.
  Preserve safe defaults in the small-terminal fallback and restore terminal
  settings after cancellation.
- Allow a bounded graceful interruption before forcing child termination.
  Real orphan-review cancellation exposed a retained pacman lock; the
  regression now verifies release before the next transaction. Queue cleanup
  answers before starting the child so an early failure cannot kill pacpin
  with SIGPIPE and hide its cleanup result or restart advice.
- Make the repository priority editor fit narrow windows and long lists.
  Add page/Home/End navigation and require terminal output as well as input.
- Prefer the measured live terminal width over inherited COLUMNS, which can
  be stale after resizing. Keep COLUMNS as a redirected-output fallback.
- Respect the selected privilege tool in refresh, forwarding, removal, keep,
  orphan cleanup and rollback. Use explicit supported paru override arguments
  and reject unsupported helper overrides. Never use sudo validation flags
  with pkexec/doas. Preserve regular-user makepkg and helper execution.
- Make cleanup capabilities opt-in for integrations. Show each integration's
  cleanup heading before executing it, serialize terminal ownership, and
  check its outcome. AUR-only cleanup excludes unrelated integrations.
  Cleanup dry runs preview operations without executing them.
- Warn when history cannot be recorded or ephemeral Flatpak removal fails.
  Signalled query/forwarding and ephemeral provider commands no longer return
  a successful exit status. Wizard hints reflect whether aliases were enabled.
- Add explicit transaction/UI contracts and the required regression matrix
  to AGENTS.md, retaining VM access, shutdown and installed-binary sync rules.
- Require dedicated boundary APIs for package managers and other external
  effects, with shared process/cancellation handling and resource-cleanup
  assertions. Record legacy bypasses rather than claiming a module name
  alone makes every caller use the common contract.

## Component review

This was a source review of manager scope, input/output ownership, status and
errors, privilege routing, temporary storage, and success/cleanup ordering.
It was not an exhaustive security audit or a claim that every command was
executed in the VM.

| Component | Review and verification |
| --- | --- |
| CLI parsing, dispatch and query forwarding | Scoped runtime tool overrides, read-only vs privileged forwarding, error exit status; parser and privilege unit tests |
| Native/AUR install and upgrade | Prepared solver plan, counts, dependencies, stage completion, cache scope, failure ordering and selected helper/tool; real guest pacman/paru/makepkg transactions |
| Check, resolver and ALPM access | Shared plan summary, implicit dependency upgrades, ignored targets, already-current targets and plan signature; prepared-transaction tests and guest dependency fixture |
| Sources: direct recipes, catalogs and install/provider | Source ownership, exact approved versions, checksum, RAM/disk scratch, cleanup and prefetch lifecycle; source unit tests, real local catalogs and guest package creation |
| Progress and selectors | Controlling terminal, concise phases, preserved logs, concurrency, completed rows, long inventory, Enter, resize and signals; real Kitty screen/scrollback plus PTY/unit checks |
| Restart inspection | Stacked wrapped fields, unbroken shell arguments and selected tool; real deleted-library mappings in disposable user/system units at 80/40/32 columns |
| Clean, keep and orphans | Per-manager capability/scope, confirmation, dry run, lock checks, exit status and truthful cleanup; command tests and controlled guest failures |
| Flatpak/Nix/Pipx adapters | Scoped rows, cleanup support and ordering, upgrade failures; executable fixtures, not actual application upgrades; see remaining findings |
| Run/try, archive and downloads | Ephemeral cleanup reporting and signal exit status; checksum/extraction/child-lifecycle unit tests and controlled guest provider commands |
| Capture/rebuild and manifests | Exact target grouping, scope/confirmation, ordinary installer delegation, final success after checked operations; existing manifest/CLI unit tests and source review |
| Config, wizard, aliases, pins, delays and repository order | Atomic/config-lock operations, success after writes, configured alias hints, interactive width/navigation; existing tests plus narrow priority-editor cancellation |
| History and rollback | Selected tool execution/display and journal-write warning; existing journal tests; partial commits remain a finding below |
| Utilities and AUR metadata | Display/argument validation and bounded metadata parsing; existing unit tests; incomplete remote checks remain a finding below |

## Remaining findings and limits

1. Several external metadata checks still convert command, parsing or network
   failures to an empty update list. Source/AUR checks warn, but a global
   “fully up to date” message can overstate what was checked. A subsequent
   change should give checks a typed incomplete/error result and make the
   global summary reflect it. Nix's query/upgrade profile selection also
   deserves its own real-profile fixtures; its fallback is currently broad.
2. After a failed native commit, package database versions alone cannot prove
   that partially extracted files are intact. This change warns and refuses
   later upgrades; it does not reconstruct a complete partial-transaction
   journal or repair already damaged packages.
3. Capacity checks are conservative estimates, not reservations. They do not
   promise exact Btrfs allocation, snapshots, quotas, inode availability on
   native destinations, concurrent writers, split `/usr` filesystems or hook
   output. The source scratch checks retain their separate quota/inode/RAM
   requirements. No automatic snapshot deletion, balance or partition repair
   was added.
4. Native background downloads expose phase status when pacman does not emit
   progress on a pipe; no fabricated aggregate byte count is shown. Available
   foreground counters/percentages are retained. Narrow native windows keep
   pacman's stage text and avoid its oversized progress rows.
5. Some secondary pin/history/orphan tables still rely on terminal soft
   wrapping for long rows. The selectors preserve complete names and the
   restart advisory uses explicit wrapping; secondary tables have not all
   received a dedicated stacked layout.
6. Ephemeral Flatpak cleanup failure now warns truthfully, but a successful
   application run can still return success while its cleanup warning says
   the temporary application remains. A follow-up can expose a combined run
   and cleanup outcome rather than relying on a Drop guard.
7. External interactions are not yet fully centralized. `pm` is the intended
   native boundary, but upgrade orchestration still constructs native/helper
   commands, and keep/orphan/rollback paths construct their own subprocesses.
   Refresh, integration checks, and ephemeral adapters also use separate
   status/output paths. The common cancellation contract has been corrected;
   a later consolidation should move these legacy callers onto dedicated
   boundary APIs and a shared tracked process runner. AGENTS.md now requires
   this ownership model for package managers, network, files, services and
   persistence; it does not claim that this migration is already complete.
8. With ALPM CheckSpace deliberately disabled, a read-only existing file can
   produce a nonfatal libarchive unlink warning while pacman returns success.
   Removing the old file still produced only a nonfatal unlink warning in
   this VM. The final extraction fixture instead permits file creation on
   a tiny tmpfs and exhausts that mount while writing package contents.
   This audit does not turn every ALPM warning into an error or establish
   post-install integrity of every file.
9. Known-orphan state persistence still discards write errors. A failed state
   save can make an unchanged orphan list appear new on the next run. It
   should use a checked atomic persistence API with an explicit warning;
   this source-review finding was not exercised by filling host storage.

## Verification and evidence

The disposable Garuda live VM uses a writable overlay and Kitty on Xorg,
regular user `garuda`, 5120 MiB RAM and four CPUs. Native/source operations
use real pacman and unprivileged makepkg against isolated guest databases,
caches and local fixtures. AUR cases use real paru and the small real `hello`
recipe. Flatpak/Nix/Pipx adapters and selected failure commands are controlled
fixtures and are identified as such in the verifier.

The extraction-failure fixture mounts a 64 KiB guest tmpfs over only
`/opt/pacpin-ui-native`, hiding the old disposable package's files until
unmount. It temporarily disables ALPM CheckSpace for that case so writing
the 1 MiB package file really exhausts that mount during extraction, then
restores config and unmounts in `finally`. The separate partial-commit wrapper is injected;
it is not a reproduction of a filled host filesystem. The low-space fixture
uses an empty 16 MiB guest tmpfs and a controlled RootDir query.

Reproduction, inside the disposable guest:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --aur --output /tmp/pacpin-ui-review-new
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --section audit --output /tmp/pacpin-ui-audit-new
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --section auth --aur --output /tmp/pacpin-ui-auth-new
```

Final results:

- `cargo test`: 248 passed, 0 failed, 2 ignored. Both ignored tests
  (`real_makepkg_archive_round_trip` and
  `cli_signals_remove_source_download_workspaces`) passed when explicitly
  executed in the VM with `PACPIN_TEST_BINARY=/tmp/pacpin`.
- `cargo clippy --all-targets -- -D warnings` and `cargo build --release`
  passed. Release and installed binary compared byte for byte; `pin` still
  links to `pacpin`. Binary size: 6,828,096 bytes. SHA-256:
  `5a9068b807de0df03b2a5b405bb35f4e729300c4f8858ced5094a4deb7ad3b92`.
- 50 distinct VM scenarios passed on that release: 39 transaction/UI/failure
  scenarios and 11 authentication scenarios. Authentication included native,
  Sources, AUR, mixed managers, `--no-cache`, two consecutive invocations,
  a 100-second hook exceeding the configured 90-second credential lifetime,
  and an injected session-reset failure.
- The main run's original read-only extraction fixture failed its assertion
  because pacman reported a nonfatal unlink warning. Its original result is
  retained. The corrected 17-case audit rerun used the tiny tmpfs and observed
  a real `could not extract ... (Write failed)` error with exit status 1.
  The combined review uses the latest result per scenario and records the
  superseded fixture failure explicitly; it does not describe the original
  main run as entirely passing.
- Initial sudo validation occurred before the manager footer in the sampled
  authentication frames. Those runs verify prompt counts and terminal
  preservation, not a visible authorization row during initial validation.
  The short authorization phase mapping is covered by unit tests.
- Guest pacman configuration matched its backup, the temporary authentication
  policy was removed, and the fixture mount was gone before shutdown.
  VirtualBox reported `VMState="poweroff"` at
  `2026-10-07T10:03:52.403Z`; `list runningvms` was empty.

Evidence:

- Combined results with provenance (local verification artifact)
- Original main results (local verification artifact),
  corrected audit (local verification artifact), and
  authentication results (local verification artifact)
- Actual completed-Pacman/running-Sources screenshot (local verification artifact)
- Orphan selector with native download footer (local verification artifact)
- Real extraction failure and scoped cleanup messages (local verification artifact)
- Dependency details followed by final summary (local verification artifact)
- Narrow repository editor (local verification artifact)

## Host state

The authorized pacman archive cleanup and stale empty-directory removal
completed. The approved thumbnail cache was removed; browser profiles and
cookies were not touched. Root still had approximately 35 MiB free afterward.
The repository resides on a different filesystem with approximately 20 GiB
free, so release builds do not require gigabytes of root scratch space.

Later readings fluctuated between a few MiB and roughly 17 MiB free. `/home`
shares the same 29 GiB Btrfs filesystem as `/`; free space on `/path/to/workspace`
does not increase root capacity. Readable directory measurements included
approximately 12 GiB in `/usr`, 5 GiB in system Flatpak, 2.8 GiB in user
Flatpak, 3 GiB in Gemini/Antigravity data and 2.3 GiB in Codex data. These
directory figures are not additive physical usage on compressed/shared Btrfs.
The native cache was still empty. The 6.5 MiB pacpin release does not explain
the overall shortage. The cause of the short-term fluctuations was not
established; no additional application data was removed for this inspection.
The final reading after VM shutdown was approximately 10 MiB free.

Host pacman integrity output (local verification artifact)
confirmed SHA-256 mismatches in both main
WebKit libraries and numerous program/data files following the interrupted
upgrade. Cache deletion did not repair these files. No host package recovery
was performed. More usable root space and an authorized package recovery
operation are still required. The new refusal checks prevent knowingly
starting another plan with this little space; they do not make the existing
damaged installation healthy.
