# Orphan Enter handoff and background terminal ownership — 2026-10-15

## Reproduction

With the original release (`ee56cb7c35648eec584ce35f00c9dfdda16492d15ce9544d3dfcd24952fb94de`),
the direct Kitty terminal test with 19 orphans and 10 default selections passed
using the guest's classic sudo 1.9.17p2. It then reproduced the user's stall
using a private copy of the host's sudo-rs 0.2.15 in the same disposable guest:

1. Approve the plan and press Enter once on the unchanged default selection.
2. The selection confirmation appears, followed by the repository-download wait.
3. The footer remains at `Pacman: Verifying packages (100%, 1/1)` after pacman
   has exited. pacpin is waiting for the background sudo process to finish.
4. A second Enter releases sudo and the transaction continues.

The second Enter was a manual diagnostic action, not an automated passing test.
The archived reproduction result explicitly records that rescue as a failure.
The helper copy was installed only after the user explicitly authorized its
temporary guest root ownership and setuid permissions. Guest administrative sudo
was retained throughout. No host package transaction or privilege-policy change ran.

## Cause and change

`progress::start_background` redirected stdin to `/dev/null` and output to a
private PTY but left the download helper in the foreground process group.
sudo-rs still opened `/dev/tty`. During the stall its parent was blocked in
`read(7, ..., 8192)`, with descriptor 7 referring to `/dev/tty`; its monitor was
waiting on its backchannel. The first Enter had already completed the selector.
Redirecting stdin had not isolated the helper from terminal input.

Background commands now start in their own process group while retaining the
invoking session and controlling terminal for sudo's ticket lookup. The shared
executor tracks that group for cancellation. Foreground manager input and
authentication retain their existing transport.

The new Rust regression checks the background process's group and retained
session. It failed against the original implementation and passes with the
change. Development contracts now require testing the actual privilege helper,
unchanged default selections, shell job control, both download timings, failure,
cancellation and long reviews. A guest with a different sudo implementation had
missed this issue in earlier verification.

## Verification

Fixed release and installed binary SHA-256:

`f4e0c967fdad94c6ce6d244fc6ab72aee1d68f2674f66b716240ff1651cd79ac`

- `cargo test`: 342 passed, 0 failed, 2 ignored.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed.
- Explicit ignored tests with `PACPIN_TEST_BINARY`: 2 passed.
- Atomic installed-binary replacement and `cmp`: passed; `pin` preserved.

## Disposable VM sequence audit

Garuda live overlay under VirtualBox, 3584 MiB/four CPUs, regular `garuda` user,
real Kitty screen/scrollback, isolated ALPM database/cache, and real pacman and
makepkg. The direct-terminal tests cover both a normal shell script and shell
job control, with one physical Enter for unchanged default selections.

**69 selected VM scenarios passed.** The evidence validator checks every
selected result, its exact release hash, cleanup state and final VM power state.

| Suite | Checks | Assertions |
| --- | ---: | --- |
| Original direct-terminal shell setup, fixed release | 1 | Same sudo-rs reproduction setup; one Enter, ten removals, nine unchecked optionals retained |
| Flag/scope/manager matrix | 23 | Defaults, no-cache, RAM/storage, keep-going, autoremove, noconfirm, refresh, Sources/external scopes, held updates and dry runs |
| sudo-rs timing/failure/interruption | 8 | Enter during/after downloads, 65-second review, narrow window, search, None, download failure and Ctrl-C |
| sudo-rs authentication | 3 | One password through prefetch/install/Sources/orphans/cache; fresh password despite an existing valid ticket; 100-second menu review with a 90-second ticket |
| Classic sudo authentication | 3 | Same transaction, fresh-invocation and long-review cases |
| Real AUR sequences | 4 | AUR-only scope, native+AUR, AUR+Sources, and all three with storage/no-cache; real Paru and AUR hello 2.12.1-2 |
| Named manager footers | 7 | Flatpak/Nix checks and pending/completed rows, no-work/failure, AUR pending during native download, resize and redirected output |
| Aggregate native percentages | 2 | Unequal real archives in foreground/background; transaction download totals and 50% after the first of two installs |
| Resize/wrap restoration | 6 | Repeated mixed transactions, native-only, Sources-only and terminal wrap mode |
| Orphan retry/persistence | 12 | AUR failure, removal failure, no-update retry, scope/noconfirm preservation, decline, migration, persistence failure and direct cleanup |

Flatpak/Nix commands are **controlled adapters**, not actual application updates.
Their named stages and literal-command/check/verify/cache lifetimes are tested.
The AUR failure/retry cases use a controlled failing helper; the four AUR
transaction cases use real Paru, makepkg and the actual hello recipe.

One forced-RAM Sources case correctly refuses the build because MemAvailable is
below the mandatory 2.5 GiB reserve. That is an expected capacity result, not a
successful RAM build. Native installation completes, its no-cache cleanup runs,
Sources remains at the old version, the overall exit is nonzero, and selected
orphans remain pending. Download failure and interruption likewise preserve
orphans, installed versions, terminal settings and lock cleanup as appropriate.

The supplemental local Kitty screen-engine check passed prompt visibility after
resize, exact scrollback retention and a stable main-output anchor through four
quiet footer phases. It supplements the actual guest CLI tests.

Two initial authentication-fixture assumptions were corrected: sudo-rs uses its
actual PAM prompt rather than the requested SUDO_PROMPT, and a no-work second
invocation correctly needs no password. The earlier no-work fixture's failed
expectation is retained in `auth-long-and-invalid-fixture`; only its successful
long-review result enters the table. The corrected fresh-invocation fixture
reseeds work and proves the previous terminal ticket is valid before starting
the next command. Neither discarded assumption is treated as a product bug.

### Reproduce

Transfer `verify_upgrade_sequences_vm.py`, `verify_upgrade_ui_vm.py`,
`verify_capabilities_vm.py`, `export_vm_evidence.py` and the release into the
disposable guest, then prepare Kitty as described in `docs/development/vm.md`.
Run the sections serially with fresh output paths:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_sequences_vm.py /tmp/pacpin \
  --sudo-dir /usr/local/lib/pacpin-sequence-sudo --job-control \
  --section matrix --output /tmp/pacpin-sequence-matrix-new
```

Other sections are `timing`, `auth`, `auth-fresh` and `aur`. Omit `--sudo-dir`
for the guest's original sudo; omit `--job-control` for the original direct-shell
reproduction. The private helper must already be explicitly authorized and
prepared; the verifier does not install privileged executables.

Screens, frames, versions, terminal modes, exact release hashes and results are
in orphan-handoff-evidence (local verification artifact). This is a representative
sequence matrix, not an exhaustive proof of every possible flag combination or
external backend. Real pkexec password-agent behavior was not exercised by this
sudo-specific fix; pkexec does not use the affected background-download branch.

The temporary privileged helper and its directory were removed; the original
guest sudo remained installed. Guest pacman.conf and the temporary authentication
policy were restored, with no package processes or ALPM locks left. Evidence was
exported before shutdown. VirtualBox confirmed `VMState="poweroff"` at
`2026-10-15T18:20:18Z`, releasing the guest's RAM. The graceful shutdown completed
before the fallback poweroff request, which reported that the VM was already off.

See summary.json (local verification artifact),
clean-state.json (local verification artifact),
vm-state.txt (local verification artifact) and
local check logs (local verification artifact). The local changes remain
uncommitted; no push or tag change was performed.
