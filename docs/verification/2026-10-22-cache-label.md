# Cache label clarification

Renamed the upgrade summary row to **Cache Before Update** to identify when
its cache estimate is measured. Updated the README, alignment fixtures and
real-terminal verifier expectations. Calculation, manager scope and cleanup
behavior are unchanged. Version remains 3.3.0-alpha.1.

## Verification

- `cargo test`: 412 passed, 0 failed, 2 ignored.
- Strict all-target Clippy and release build passed.
- Both opt-in tests passed against the release binary.
- Python verifier syntax and `git diff --check` passed.
- Release atomically synced to the installed binary, byte equality checked and
  the `pin` symlink preserved. SHA-256:
  `8bdb7ef87a2089f7ee20b07b61920a91def5f20ef045332d9bc55ec851dc7f35`.

Two real-terminal cases passed in the user-selected disposable Garuda/VirtualBox
live guest, using regular guest execution and Kitty/Xorg. Both 80- and 44-column
previews displayed the new wording; Kitty's independent width engine confirmed
matching card border columns. Approval was declined and installed fixture
versions stayed unchanged. The verifier exposes these existing cases through
`--section cache-cards`; the full review still includes them.

Guest and installed release checksums matched. Guest pacman configuration was
restored and no fixture database lock remained. Raw screen/scrollback captures
and logs are local in ignored `docs/verification/cache-label-evidence/`.
No host package operations, GitHub push or tag changes were performed.
The guest shut down cleanly; VirtualBox confirmed `VMState="poweroff"`.
