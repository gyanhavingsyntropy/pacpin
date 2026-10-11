# Footer coverage audit — 2026-10-12

The reported missing step could not be identified from a saved screenshot. A
trace of the full upgrade invocation found paths outside the shared child
executor: refresh, checks, plan approval, orphan review without downloads,
dependency reasons/history, source extraction/scratch release, hook verification
and restart inspection. Failed manager rows also disappeared during continuation.

## Coverage

| Work actually running | Footer description |
| --- | --- |
| Native repository refresh | Repositories: Refreshing repositories |
| Supported external metadata refresh | Metadata: Refreshing external metadata |
| Manager/update/dependency/space checks | Checks: brief description of the current check |
| Final plan approval | Review: Awaiting plan approval |
| Orphan selection, with or without downloads | Review: Choosing orphan cleanup |
| Concurrent native download during selection | Separate Pacman download row |
| Source preparation/extraction/scratch release | Sources: current preparation phase |
| Native/AUR/Sources/integration installation | Existing manager row and actual progress |
| Dependency install reasons and upgrade history | Records: current recording operation |
| Post-update condition checks | Checks: Verifying hook conditions |
| AUR rebuild triggered by a hook | AUR build row plus Hooks context row |
| Orphan removal | Existing Orphans removal row |
| Native/AUR cache cleanup | Cache: manager-specific cleanup phase |
| Restart inspection | Restart: Checking restart requirements |

Native refresh, prefetch authorization and dependency reasons now use typed `pm`
operations with the shared tracked executor. Non-child phases share footer
ownership, resize handling and terminal restoration. Orphan selection is wrapped
at its UI boundary, covering direct cleanup and no-update retries as well as the
upgrade selector. A nested phase does not replace the existing terminal owner.

Failed managers remain marked during later work; successful managers are checked
only by their verified completion paths. Hook completion requires an action that
actually ran. Metadata refresh is an explicit provider capability: Flatpak and
Nix opt in; Sources, Pipx and future providers default to unsupported. Concurrent
metadata work uses a context row while native refresh runs. Short terminals keep
active rows and recent failures, limiting older completion history so ordinary
output retains space. Prompts and original manager logs remain in the main area.

## Verification and limits

Required `cargo test`, `cargo clippy --all-targets -- -D warnings` and
`cargo build --release` were run with `$HOME/.cargo/bin` on PATH. Test scratch
was placed on the repository HDD via TMPDIR. The release was atomically synced
to `$HOME/.local/bin/pacpin`, checked with `cmp`, and `pin -> pacpin` preserved.

The PTY regression exercises non-child phases, nested ownership, failed AUR and
successful native history, hook rebuild context, orphan review alongside native
downloads, resize, inherited authentication input/session, CRLF preservation and
terminal restoration. Separate unit checks cover short-footer row priority and
explicit metadata capabilities.

The disposable Garuda VM could not be accessed: the VM storage server did not respond, VirtualBox
could not resolve its configured VM, and the user confirmed the VM storage server is unavailable.
**Actual Kitty screen/scrollback and guest authentication checks remain pending.**
No VM was started and no host package installation/removal was used for testing.
Compilation and PTY checks are not claimed as rendered-screen verification.

The existing disposable-only `scripts/verify_housekeeping_vm.py` was extended with
orphan review without downloads, checks/records/restart and source scratch phases,
and failed AUR history during Sources continuation. Run these checks when the VM storage server is
available, sequentially with the existing UI/authentication suites:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_housekeeping_vm.py /tmp/pacpin \
  --output /tmp/pacpin-footer-review-new
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --section audit --output /tmp/pacpin-footer-audit-new
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --section auth --output /tmp/pacpin-footer-auth-new
```

Local build logs are in `target/maintenance/footer-20261012/`. Older unrelated
command handlers still have external-interaction bypasses; this audit does not
claim they have all been migrated.

Final result: **284 passed, 0 failed, 2 existing explicit-only tests ignored**.
Strict Clippy, release build, Python verifier syntax and `git diff --check` passed.
Release and installed SHA-256:
`3fdbde0ad3f20f558e4eeac1348354a66e3d04214247b2a74fb2fc49eb695266`.
