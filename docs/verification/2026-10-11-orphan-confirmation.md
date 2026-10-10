# Orphan confirmation and download wait — 2026-10-11

## Report and cause

The user reported that the orphan checkbox list remained visible after Enter,
followed by an empty line and no progress. The upgrade path starts native
downloads while the user reviews orphans, drops the selector footer on return,
then previously blocked in `BackgroundCommand::wait` without displaying status.
This explains a silent interval when Enter precedes download completion; it does
not establish that every reported stall has the same cause.

The earlier VM fixture served a 1 MiB package at 64 KiB per 35 ms and confirmed
after a 0.6-second selector review. Downloads could therefore finish before
Enter, leaving this transition untested.

## Changes

- Confirm the selected orphan count, or explicitly state that cleanup was skipped.
  Removal after an upgrade remains conditional on upgrade success.
- Keep a native download footer visible while awaiting a still-running child.
  Completed downloads return immediately without a misleading wait/footer.
- Preserve real child exit status, background state cleanup, normal input mode,
  and the shared signal path. Footer rendering errors cannot terminate the child.
- Leave blank lines between the complete inventory, default-selection explanation,
  and checkbox controls.
- Document this transition in AGENTS.md and strengthen the disposable VM verifier
  with slower downloads, confirmation after download completion, and interruption
  during the post-selection wait.

## Executed verification

- `cargo test`: **249 passed, 0 failed, 2 ignored**.
- `cargo clippy --all-targets -- -D warnings`: passed without warnings.
- `cargo build --release`: passed.
- The PTY regression confirms an orphan with one Enter while a controlled shell
  download remains active, observes later 75% progress, preserves failure status 7,
  verifies input/output terminal settings restoration and background state cleanup,
  checks section separators, and exercises an already-finished successful child.
  These fixtures perform no host package installs or removals.
- Python AST parsing of the VM verifier and `git diff --check`: passed.
- Atomically installed the release at `$HOME/.local/bin/pacpin`, verified byte
  equality, and preserved `$HOME/.local/bin/pin` as its symlink.
- Installed SHA-256:
  `9aa4b2e84046d47e2240ec3c3359740e11f936d2587acab9f1c9cf8a6666fae0`.

## Pending verification

The Garuda ISO on `/path/to/vm-storage` returned `No such device`. The user confirmed the VM storage server is
unavailable. No VM was started. Real Kitty screen/scrollback checks and the updated
VM cancellation/database-lock scenarios are **pending**, not passed. The two
existing VM-dependent ignored tests were not executed during this change.

When the VM storage server is available, run `scripts/verify_upgrade_ui_vm.py` with `--section audit`
inside the disposable guest, preserve its evidence, inspect the rendered
post-Enter wait, and shut the guest down afterward. Compilation and PTY output
do not establish that the rendered terminal is correct.

## Separate Antigravity/Codex question

The user also reported an Antigravity storage failure. Host metadata identifies
Antigravity as a foreign/AUR package and Codex as a configured Sources package.
The upgrade orchestrator skips external upgrades after an AUR failure. Codex
remains `0.162.0alpha.16-1`; its installed size is 430.09 MiB. Root free space was
approximately 202 MiB when checked. This change does not alter failure ordering,
retry packages, or install Codex. No recovery feature was developed.

## Follow-up verification

the VM storage server became available. The later disposable Garuda audit verified orphan Enter
while downloads were active, Enter after downloads completed, Ctrl-C during
selection and the post-selection wait, terminal restoration, and database lock
release. See [the runtime/hooks verification report](2026-10-11-runtime-hooks.md)
for the executed checks, evidence and confirmed VM shutdown.
