# AUR install pin intent and VCS version regression — 2026-10-15

## Reproduction and cause

The unqualified AUR fallback in `pm/install.rs` still appended an AUR pin
after routing a missing native target to the helper. Helper routing had been
treated as user pin intent. The current code therefore did not establish the
previously claimed fix. Dependencies discovered by the helper were not added
by that branch, but explicitly selected companions were.

Separately, the shared resolver synchronized pinned AUR candidates to their
advertised metadata version, including downgrades. The host had both
`mcpelauncher-linux-git` and `mcpelauncher-ui-git` pinned to AUR. Installed
versions were `1.8.0.r0.gef9ceb8-1` and `1.8.4.r0.g779d057-1`; the reported
metadata candidates were `1.2.0.r0.gfe07760-1` and `0.15.0.r0.g8ba3a05-1`.
VCS builds recalculate pkgver, so this comparison did not establish that a
downgrade was available or appropriate.

## Changed ownership and behavior

- Private `pm/pinning.rs` derives repository pin intent from original qualified
  requests and explicitly selected repository companions. Confirmation filters
  that intent against successfully installed targets. AUR fallback and helper
  discovery do not add intent. Failure and dry-run paths do not persist pins.
- Existing exact/wildcard pins are retained; historical accidental pins cannot
  safely be distinguished from deliberate ones. Sources keep their separate
  ownership/persistence contract. External integrations do not use this path.
- One typed `VersionChange` policy is used in both native and AUR resolution.
  For AUR packages whose name or package base has a standard VCS suffix
  (`-git`, `-svn`, `-hg`, `-bzr`, `-darcs`, `-cvs`, `-fossil`), lower advertised
  versions do not trigger downgrades and equivalent versions do not trigger
  pin-sync rebuilds. Higher versions still trigger upgrades. Stable AUR and
  native repository pins retain their existing synchronization behavior.
- This does not add upstream commit discovery when AUR metadata is unchanged,
  and unconventional VCS packages without a recognized name/base suffix are
  outside this classification.
- README, architecture map and the mandatory manager contract linked from
  AGENTS.md document the policy and its required regression checks.

## Local verification

With `$HOME/.cargo/bin` on PATH:

- `cargo test`: **341 passed, 0 failed, 2 ignored** (8 new regression tests).
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed.
- `PACPIN_TEST_BINARY="$PWD/target/release/pacpin" cargo test -- --ignored`:
  **2 passed**, including real makepkg archive round-trip and signal cleanup.
- Installed release replaced atomically; `cmp` passed. The `pin` symlink was
  preserved. Release, installed executable and guest test executable SHA-256:

  `ee56cb7c35648eec584ce35f00c9dfdda16492d15ce9544d3dfcd24952fb94de`

- Read-only host `pacpin check --aur` reported no updates in scope with the
  existing launcher pins preserved. No host package install/removal/upgrade
  was performed.
- `git diff --check`: passed. Verification preceded the requested local commit.

Logs and the read-only host plan are in aur-pinning-evidence/local (local verification artifact).

## Disposable VM verification

Garuda live ISO under VirtualBox, 3584 MiB/four CPUs, regular `garuda` user;
real Kitty screen/scrollback and recorded terminal settings. Isolated ALPM
database/cache; guest pacman.conf restored in each suite's `finally`.

Run inside the guest after transferring the release and both verifier scripts:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_aur_pinning_vm.py /tmp/pacpin \
  --output /tmp/pacpin-aur-pinning-20261015
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_aur_pinning_vm.py /tmp/pacpin \
  --real-paru-only --output /tmp/pacpin-aur-pinning-real-20261015
```

**7 cases passed:**

| Case | Evidence |
| --- | --- |
| Bare AUR fallback | Successful install creates no pin |
| Explicit `aur/hello` | Target pinned; automatically installed dependency unpinned |
| Explicit native repository | Repository pin retained after real upgrade |
| Bare native target | Real upgrade creates no pin |
| Helper exits 23 | Invocation fails and creates no pin |
| Dry run | No install or new pin |
| Real Paru bare AUR install | GNU hello built/installed as `2.12.1-2`; no pin created |

The first six cases use a **controlled helper fixture** with real makepkg and
pacman transactions. It deliberately installs an unrequested package with
`--asdeps`; assertions check both its dependency reason and absence from pins.
They preserve pre-existing exact and wildcard rules, check ALPM lock cleanup,
and verify terminal restoration and manager row scope. The seventh uses real
Paru, the upstream AUR recipe and GNU source, without replacing the helper.
The heavy Minecraft launcher packages were not rebuilt.

Compact evidence archives include results, binary identity, terminal snapshots,
scrollback, transcripts and statuses:

- Controlled helper cases (local verification artifact)
- Real Paru case (local verification artifact)
- Rendered real Paru completion (local verification artifact)

Guest pacman.conf restoration, absent fixture/default ALPM locks, and no test
sudoers policy were confirmed. ACPI shutdown completed;
`VBoxManage showvminfo` returned **VMState="poweroff"**, and `list runningvms`
was empty.

The source-compilation task's final 13-case rerun and shared cleanup/authentication
checks were subsequently completed separately; see
[the source-build verification report](2026-10-15-source-builds.md).
