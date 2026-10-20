### Required verification after code changes

Use `PATH="$HOME/.cargo/bin:$PATH"` for Rust commands. Run:

1. `cargo test`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo build --release`
4. Sync the release to `$HOME/.local/bin/pacpin` and verify byte equality. Preserve the `pin` symlink. Prefer atomic replacement; do not leave a failed staging file.

After editing code, verify the changed behavior and related modules/command
paths in the disposable VM specified by the user, reusing an applicable choice
from the current session. Follow [VM selection](vm.md) if the target or access
details are missing. Do not substitute a historical Garuda/VirtualBox profile
without the user's selection. Record the actual guest and tool versions in
evidence. If the selected VM is unavailable, complete independent checks and
report VM verification as incomplete.

For terminal, progress, scheduling, or manager orchestration changes, verify the
actual CLI in that VM using a real terminal emulator. Unit tests or PTY
recordings alone do not establish that the screen is correct. Check rendered
screen and scrollback as well as command logs and installed versions.

The commands below describe existing Garuda/VirtualBox fixtures, not a mandatory
distribution or hypervisor. Before using them on another selected guest, adapt
paths, accounts, guest detection and tooling while retaining explicit disposable
guest/isolation checks and the same assertions. Do not bypass an incompatible
fixture's safety guard or treat a skipped fixture as a passing check. No VM needs
to be started for a documentation-only change.

Required scenarios:

- For onboarding changes, run `PACPIN_DISPOSABLE_VM=1 python
  /tmp/verify_init_vm.py /tmp/pacpin --output /tmp/pacpin-init-review-NEW`
  inside the regular guest account with the documented real Kitty socket.
  Transfer `scripts/verify_init_vm.py` and the release first. It isolates HOME
  and XDG config/state/cache paths and performs no package operations. Check
  fresh and automatic first launch, setup alias, existing main/supplemental
  preservation, effective manager overrides, reset/backup, final decline,
  EOF/q/Ctrl-C, invalid inputs/options, individual manager selection, optional
  alias writes only after approval, concurrent edits to each config file,
  malformed files, non-TTY refusal and templates-only automation. Inspect narrow
  and normal rendered reviews plus scrollback; save results and screenshots.
  A configured command in a fixture is not permission to execute it during init.

- `scripts/verify_preparation_vm.py --section core` checks eligible preparation
  targets, per-manager footer percentages, strict operation ordering, preparation
  failure followed by normal retry, cache/forced scratch policy, unsupported
  optional preparation, dry-run effects, real native/Sources transactions,
  resize and cancellation cleanup. External adapters are controlled fixtures.
  Its `--section aur` uses real Paru/makepkg and the upstream small hello package,
  with private build paths and curl rate limiting to expose intermediate
  transfer percentages. Verify AUR-only, AUR+Sources and native+AUR+Sources,
  Sources phase gating, completed/pending rows, retained history and terminal
  restoration. Transfer the sequence/capability/UI helper scripts with it.

- `scripts/verify_source_discovery_vm.py` verifies real concurrent catalog reads
  through a small guest-only delay interposer; successful and failed packages
  sharing an alias, another alias, strict/read-only/scoped/dry-run/no-work paths,
  and Sources checks overlapping a controlled Flatpak refresh. Native/Sources
  installations use real pacman/makepkg. Assert installed versions, partial
  discovery footer warnings, terminal restoration, retained native scrollback
  and no scratch/lock leaks. Transfer it with the discovery/capability/UI scripts.

- Run `scripts/verify_discovery_vm.py` with the shared capability/UI scripts in
  the disposable real terminal. Verify failed Sources and configured manager
  checks, malformed metadata, external refresh failure without stale recheck,
  AUR RPC failure, strict versus --keep-going, scoped/all-failed/dry-run paths,
  skipped footer rows/reasons, unchanged pending orphan cleanup, eligible verified
  scripts and blocked AUR rebuild hooks,
  successful native/Sources work, successful-manager cache cleanup and no
  failed-manager actions/cache. Its external adapters are fixtures; AUR failure
  uses temporary guest-only hosts redirection, restored in finally. Unit tests
  additionally exercise a panicked check worker and typed identity collisions.
  Run both the default section and `--section extra` sequentially. The latter
  also verifies native dependency refusal, multiple failures, normal hooks and
  scoped exclusion. Discovery fixtures use a shorter artificial read delay than
  the timing/UI suite; they are not long-download timing coverage.

- Sources only: download, verify, package, install, and completion; no native-repository or AUR status.
- Native packages and Sources, with no AUR: defer source prefetch until native downloads finish, preserve pacman text through source completion, and never report AUR completion.
- Native only and AUR only: no irrelevant manager or Sources rows.
- Ordinary native and AUR installation paths when their progress handling changes.
- AUR and Sources, and native plus AUR plus Sources when those paths are changed.
- An enabled integration with no updates, and an integration without prefetch support: no misleading source-download status.
- Integration-only updates when changing their progress display. Record whether an integration executable was simulated or real; do not report simulated updates as actual application upgrades.
- Source verification or packaging failure and native failure: accurate failure state, no false success, and workspace cleanup.
- Resize, narrow terminals, final footer teardown, prompts, interruption, and redirected/non-TTY output.
- Direct default orphan confirmation with one physical Enter and no preceding search/selection changes, both during and after downloads. Include a review spanning sudo ticket renewal, a download failure and Ctrl-C during the wait. Exercise shell job control as well as a nested PTY, and compare the user's sudo implementation/version with the guest's: classic sudo and sudo-rs do not establish each other's terminal behavior. Timeout evidence must capture the screen and process groups/wait channels before cancellation; never count a manually rescued second-Enter run as passing.
- Password authentication across database refresh, native installation, and source installation in the same run. The live VM defaults to passwordless sudo, so that alone cannot verify authentication reuse. Run the verifier with `--section auth`; it temporarily requires passwords only for guest pacman, asserts exactly one prompt per transaction, and restores guest policy afterward.

Use small local catalog and package fixtures in the VM instead of multi-gigabyte real downloads. Include enough pacman and hook output to exercise scrolling. Assert that earlier lines survive footer teardown and later source output. Inspect screen captures for the reported scenarios.

Record the reproduction command, VM environment, assertions, and results in the repository. If the VM is unavailable, report that verification as incomplete; do not claim the terminal issue is fixed based on compilation or raw-output substring tests.

The reusable verifier is `scripts/verify_upgrade_ui_vm.py`. Transfer it and the release binary into the live guest, start the Kitty socket described below, then run **inside the guest**:

`scripts/verify_upgrade_sequences_vm.py` adds direct-terminal default-selection,
flag/scope/manager combinations and timing/authentication sections. Transfer it
with `verify_capabilities_vm.py` and `verify_upgrade_ui_vm.py`; use
`--section matrix`, `--section timing`, `--section auth` or `--section aur`,
a fresh `--output` directory, and `--job-control` for an interactive-shell
process-group layout. An optional
`--sudo-dir` selects an already prepared guest helper without changing the
guest's administrative sudo. Any privileged helper installation needs separate
authorization and must be removed afterward. External adapters in this suite
are explicitly controlled fixtures.

Authentication fixtures must recognize the actual visible prompt (sudo-rs PAM
can ignore SUDO_PROMPT), wait through redraws, and supply only the disposable
guest password. To test a fresh subsequent invocation, reseed actual work and
prove the previous terminal ticket is still valid before launching it. A second
invocation with no privileged work correctly needs no password; expecting one
there is an invalid fixture, not evidence of an authentication regression.

All VM suites share the terminal and temporarily replace guest pacman.conf. Run them sequentially; the shared verifier lock rejects concurrent suites. Preserve an earlier failure when chaining shell commands (`set -e`), rather than allowing a skipped `if` branch to reset the shell status to success.

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --aur --output /tmp/pacpin-ui-review-new
```

Use a fresh output directory. The verifier requires a Garuda live overlay running under VirtualBox and a regular user. It temporarily replaces only the guest pacman configuration, uses an isolated guest database/cache and small fixtures, and restores the config in `finally`. Native and source transactions use real pacman/makepkg; `--aur` also builds the small real AUR `hello` package with real paru and needs Internet access. Integration executables are controlled fixtures; Pipx metadata checks use PyPI. An explicit packaging-failure fixture simulates makepkg failure. A small read interposer slows the local source artifact so resizing and interruption can be observed without large allocations. Preserve `results.json`, relevant rendered screens, and screenshots before shutting down the ephemeral live guest.
