# pacpin development instructions

## Preserve the user's workspace and host

- Preserve intentional uncommitted changes. Do not commit, push, or change tags unless explicitly requested.
- The host root filesystem is small and often nearly full. Avoid large scratch files there; inspect disk space and available RAM before VM work or package builds.
- Do not install, remove, or upgrade host packages for verification. Use dry runs on the host and actual transactions only in a disposable VM.
- Start pacpin unprivileged. Run makepkg and AUR helpers as the regular user. Elevate only native pacman operations. Use the user's selected privilege tool; this user's administrative preference is pkexec.
- Shut down a disposable VM started for verification after the checks and confirm it has stopped, so it does not retain RAM.

## Scope every feature to the managers involved

- Derive status rows, completion messages, prefetch, cleanup, and flags from the approved transaction and the manager actually invoked.
- Do not display an AUR row or claim AUR completion when no AUR operation ran. Apply the same rule to native packages, Sources, Flatpak, Nix, and other integrations.
- Provider capabilities must be explicit and default to unsupported. Do not assume a generic integration supports source downloads or source scratch handling.
- Upgrade scope controls must exclude unrelated checks, refreshes, downloads, status rows and cleanup. `--sources-only` / `--aur-only` never refresh native repositories; AUR-required build dependencies are a distinct possible native transaction. Keep pacpin controls out of helper arguments, respecting option values and `--` targets.
- Scratch flags select temporary storage only. Forced RAM/disk must match the filesystem, retain quota/inode/RAM checks and root installation checks, and never silently fall back to the other medium. AUR build size is unknown: do not describe its starting check as a full footprint guarantee. Configure Paru through its supported clone directory and makepkg environment, unprivileged, with explicit rejection for unsupported helpers.
- Continuation is opt-in with `--keep-going`, only after positive build-failure evidence and unchanged native database/log with healthy installed dependencies. Native/unknown failures and interruptions stop the run. Providers default to unsupported; Sources validate exact approved plans, transitive and actual archive dependencies and the prepared native transaction before continuing. Keep the original failed stage and nonzero result visible; never mark it completed or purge its cache because an independent package succeeded.
- Distinguish downloading, checksum verification, packaging, installation, and successful completion. A completed prefetch is not a completed upgrade.
- Keep pacman output and hooks visible. Background workers must not write directly into a foreground manager's terminal display.
- Use the shared progress renderer for foreground installation and upgrade commands. Native, AUR, Sources, Flatpak, Nix, and Pipx should identify the active manager consistently. Add a concurrent Sources row only to native/AUR operations with actual source work; a shared renderer does not authorize sharing manager-specific flags, scratch rules, or cleanup.
- Preserve the invoking terminal session and inherited stdin across foreground commands. Sudo caches credentials per terminal; creating a new controlling PTY for each stage causes repeated password prompts. Relay output without taking ownership of authentication input or changing sudoers on the host.
- For interactive sudo operations, the user expects fresh authentication once per pacpin invocation and reuse throughout that invocation, including AUR builds. Reset only the current terminal ticket lazily before the first actual privileged operation, keep it renewed without prompting during long operations, and leave read-only commands, dry runs, other privilege tools, and other terminals unaffected. Test consecutive invocations in the same terminal with an existing ticket.

## Centralize external interactions

- Post-update hooks belong to the hooks boundary. Preview configured actions before final approval; trigger once per hook only from verified installed version changes after successful transactions. Evaluate any/all/unless conditions against the plan and verified results; unknown or failed updates cannot satisfy positive conditions. Run scripts as the regular user through shared tracked execution with literal arguments and concise footer phases. Rebuilds use the typed AUR adapter, respect scope/scratch/privilege policy, omit `--needed`, report real failure and include actual AUR work in cache cleanup. Do not recursively trigger hooks or infer script effects from its text. Ordinary installs/recovery currently do not run upgrade hooks; keep this limitation explicit until their outer orchestration supports the same contract.

- Command handlers and orchestration code must call dedicated boundary APIs instead of spawning package managers or implementing their side effects independently. Route native/AUR execution through `pm` and its helper adapter (`aur` owns AUR metadata), Sources through `sources`, and Flatpak/Nix/Pipx through their integration adapters. A module name alone does not establish centralization: inspect callers for bypasses.
- Prefer typed operations such as install, refresh, clean and remove over arbitrary command strings from callers. The boundary owns argument validation, supported flags, privilege policy and operation-specific outcomes.
- Keep process creation, tracking, waiting, cancellation, terminal ownership, privilege routing and outcome interpretation in shared execution code used by those boundaries. Boundary APIs own manager-specific arguments and policies; orchestration owns ordering and the approved plan. Do not duplicate sudo validation, cache answers, PID cleanup or success inference at individual call sites.
- Apply the same ownership rule to HTTP downloads/catalogs, filesystem/config writes, archives, journal persistence, services and sandbox cleanup. High-level functions call the responsible module's API. Do not scatter raw commands, filesystem mutation or network requests across UI and command handlers when a boundary exists.
- External APIs must distinguish success, failure, skipped/unsupported work and partial/uncertain outcomes. Preserve diagnostics and propagate failures; an empty list or completed download must not imply a successful check or install. Check child exit status and allow package managers to release locks before forceful cancellation.
- When changing an external interaction, enumerate its callers, migrate affected bypasses to the shared boundary, and test the common contract plus each affected manager path. Record remaining legacy bypasses honestly; a rule in this file does not mean they have been refactored.
- Regression checks must verify cleanup of program-owned resources such as database locks and temporary workspaces, not merely that a PID stopped. Use architecture checks for new bypasses and document any existing exceptions during migration.

## Transaction and UI contracts

- Render all requested targets, solver-added dependencies, implicit dependency upgrades, and removals before the final plan summary. Derive native counts and sizes from the prepared ALPM transaction. Counts must explicitly include new dependencies: show `41 upgrades + 7 new dependencies`, omit `+ 0`, and make the overall total unambiguous. Keep read-only check and upgrade previews consistent.
- Treat download size, final net growth, and temporary peak storage as different values. Check available root and cache space before native downloads and again before installation. `--no-cache` cleans after a successful manager transaction; it cannot eliminate peak installation space. Btrfs snapshots, open deleted files, quotas and hooks can make estimates incomplete.
- Report cache cleanup separately for each invoked manager: completed, failed, unnecessary or skipped with a reason. Check actual cleanup exit status before reporting success. Preserve failed-manager archives for recovery; successfully completed managers can clean their own caches even if a later manager fails. Never silently hide cleanup inside journal-recording conditions.
- Explicit `clean --repo` and `clean --aur` scopes exclude integration cleanup and its discovery, prompts and dry-run output. Reject conflicting scopes. Consume helper-only selectors in the native fallback and refuse AUR cleanup when no helper is available. Verify enabled integrations are excluded from both real and dry-run scoped clean commands.
- Finish orphan/cache cleanup before printing restart advice. A failed pacman commit can still change earlier packages or files; do not claim the system is unchanged based on pacman's generic abort text. Download verification is never installation success. Report skipped later managers.
- Footer rows describe phases in a few words; never echo password prompts, yes/no questions, entire command lines or arbitrary hook output. Preserve original text in the main area. Retain a checked completed Pacman/AUR row while later managers are running, only after real success.
- Flush terminal setup before spawning an interactive child: sudo can write directly to the inherited controlling terminal. Footer allocation, resize and teardown must preserve the main cursor, prompts and scrollback without inserting padding/newlines on every phase. Password tests must wait through redraws and check the rendered prompt before typing, including a resize while it is waiting; finding the prompt in raw output is insufficient.
- Defer or suspend bottom-anchored footer rows while authentication awaits input, and resume when actual operation output starts. Sudo may keep the inherited tty in raw mode throughout pacman execution; disabling echo alone must not suppress the entire transaction's progress. The supplemental Kitty screen-engine fixture is scripts/verify_footer_screen.py; its checks must use explicit exceptions because Kitty's embedded Python can disable assert statements. It does not replace the disposable VM scenarios below.
- Pacman percentages describe the whole current stage: byte-weight all uncached downloads (or use its aggregate Total bar), and derive installation/removal/verification progress from the full package counter. Never report an individual package's bar as the transaction percentage. Preserve these metrics in narrow footers; omit unavailable totals rather than guessing from ambiguous truncated labels.
- Audit the entire invocation against the footer, including repository refresh, manager discovery/update checks, dependency and space checks, plan approval, orphan review with or without downloads, installation, dependency reasons/history, hook verification/rebuild context, orphan removal, cache cleanup and restart inspection. Shared execution alone does not cover work before or after a child process. Display actual concurrent work in separate rows, retain failed managers during continuation, distinguish native/AUR cache phases, and bound history rows to preserve the active stages and normal output in short terminals. Record unavailable VM checks explicitly.
- Background repository downloads during orphan review must appear in the shared footer and must not own selector input. Lists must preserve their complete inventory in scrollback, show viewport position and usable navigation, fit narrow terminals, and confirm on one Enter even when searching. Small-terminal fallback must retain safe default selections.
- After orphan selection confirms, print a brief selection result and keep repository download progress visible while awaiting the download process. Test Enter while downloads are still running, after they have already finished, and before a download failure. A test that finishes downloading before Enter cannot verify this transition. Keep input and Ctrl-C working throughout the wait, and verify the actual child status and lock cleanup.
- Remember inventory separately from pending orphan cleanup. Persist confirmed selections before transactions; failed/skipped removal and interruption must re-offer cleanup on a later full upgrade, including when no updates remain. Revalidate and ask again rather than treating stored names as removal authorization. Read-only listings, --noconfirm without autoremove and scoped runs must not acknowledge pending cleanup. Verify a second invocation with an unchanged known list after both AUR failure and removal failure, plus explicit decline and legacy-state migration.
- Orphan removal and cache cleanup are real footer steps owned by their typed `pm` APIs and shared tracked executor. Show completion only after successful child exit, retain earlier completed managers, and omit stages with no work. Keep automatic cache input intact through the renderer and verify one sudo prompt per invocation with cleanup enabled.
- Validate orphan selections as a complete removal set using ALPM version/provides checks. Keep dependencies of retained packages (including unchecked optional orphans) and their transitive dependencies; never implicitly add the retained parent or disable dependency checks. Use nonrecursive removal for the confirmed set so unchecked dependencies are not silently added. Record only actual removal targets, persist the adjusted pending set before removal, and skip orphan cleanup after any failed upgrade, including `--keep-going` continuation.
- Full native cache cleanup may remove inactive immediate `download-*` directories inside configured canonical cache roots after checking the ALPM lock and staleness. Never traverse symlinks or filesystem boundaries. Recent nonempty downloads and non-full cleanup retain their contents; report incomplete cleanup. AUR-only cleanup must not scan or alter native cache directories. Test a nonempty directory owned by pacman's sandbox user, symlink targets, recent downloads, cancellation and failure.
- Interrupt tracked children gracefully before forcing termination so pacman can release its database lock. Test Ctrl-C during active native prefetch, terminal restoration, lock release and a subsequent transaction. Never blindly delete pacman's lock to hide a cancellation bug.
- Sudo may give the terminal foreground to pacman, so pacpin itself may not receive Ctrl-C. Pacman's exact C-locale interruption diagnostic must enter the shared cancellation path even if it exits zero after an interrupted hook. Do not race the signal handler with success reporting or another operation. Verify cancellation during a real ALPM commit with its lock held, not only a sleeping wrapper before pacman starts.
- Feed automatic cache-clean answers without risking SIGPIPE when a helper exits early; preserve and report the child's actual cleanup exit status. Verify authentication reuse with --no-cache as well as installation stages.
- Respect the configured/runtime privilege tool in refresh, install, remove, orphan removal, keep, rollback and forwarding paths. Do not pass sudo-only validation/noninteractive flags to pkexec or doas. Helpers and makepkg remain unprivileged.
- Back these contracts with regression tests and real terminal checks in the disposable VM: new-dependency counts and ordering, long/narrow selectors, one Enter, background download status, completed manager rows, failed native extraction with possible partial application, failed cleanup exit codes, cleanup ordering and storage refusal. Record which failures were injected; a native download failure does not exercise extraction failure.

## Required verification after code changes

Use `PATH="$HOME/.cargo/bin:$PATH"` for Rust commands. Run:

1. `cargo test`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo build --release`
4. Sync the release to `$HOME/.local/bin/pacpin` and verify byte equality. Preserve the `pin` symlink. Prefer atomic replacement; do not leave a failed staging file.

For terminal, progress, scheduling, or manager orchestration changes, also verify the actual CLI in the disposable Garuda VM using a real terminal emulator. Unit tests or PTY recordings alone do not establish that the screen is correct. Check rendered screen and scrollback as well as command logs and installed versions.

Required scenarios:

- Sources only: download, verify, package, install, and completion; no native-repository or AUR status.
- Native packages and Sources, with no AUR: defer source prefetch until native downloads finish, preserve pacman text through source completion, and never report AUR completion.
- Native only and AUR only: no irrelevant manager or Sources rows.
- Ordinary native and AUR installation paths when their progress handling changes.
- AUR and Sources, and native plus AUR plus Sources when those paths are changed.
- An enabled integration with no updates, and an integration without prefetch support: no misleading source-download status.
- Integration-only updates when changing their progress display. Record whether an integration executable was simulated or real; do not report simulated updates as actual application upgrades.
- Source verification or packaging failure and native failure: accurate failure state, no false success, and workspace cleanup.
- Resize, narrow terminals, final footer teardown, prompts, interruption, and redirected/non-TTY output.
- Password authentication across database refresh, native installation, and source installation in the same run. The live VM defaults to passwordless sudo, so that alone cannot verify authentication reuse. Run the verifier with `--section auth`; it temporarily requires passwords only for guest pacman, asserts exactly one prompt per transaction, and restores guest policy afterward.

Use small local catalog and package fixtures in the VM instead of multi-gigabyte real downloads. Include enough pacman and hook output to exercise scrolling. Assert that earlier lines survive footer teardown and later source output. Inspect screen captures for the reported scenarios.

Record the reproduction command, VM environment, assertions, and results in the repository. If the VM is unavailable, report that verification as incomplete; do not claim the terminal issue is fixed based on compilation or raw-output substring tests.

The reusable verifier is `scripts/verify_upgrade_ui_vm.py`. Transfer it and the release binary into the live guest, start the Kitty socket described below, then run **inside the guest**:

All VM suites share the terminal and temporarily replace guest pacman.conf. Run them sequentially; the shared verifier lock rejects concurrent suites. Preserve an earlier failure when chaining shell commands (`set -e`), rather than allowing a skipped `if` branch to reset the shell status to success.

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --aur --output /tmp/pacpin-ui-review-new
```

Use a fresh output directory. The verifier requires a Garuda live overlay running under VirtualBox and a regular user. It temporarily replaces only the guest pacman configuration, uses an isolated guest database/cache and small fixtures, and restores the config in `finally`. Native and source transactions use real pacman/makepkg; `--aur` also builds the small real AUR `hello` package with real paru and needs Internet access. Integration executables are controlled fixtures; Pipx metadata checks use PyPI. An explicit packaging-failure fixture simulates makepkg failure. A small read interposer slows the local source artifact so resizing and interruption can be observed without large allocations. Preserve `results.json`, relevant rendered screens, and screenshots before shutting down the ephemeral live guest.

## VM access

Use the user-specified disposable VM and keep access details in an ignored local profile.

