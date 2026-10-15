### Required verification after code changes

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
