# Directory Sources and temporary cleanup verification — 2026-10-24

## Scope

This change adds managed directory installations for Sources recipes and a
shared `--cleanup=temporary` policy. A directory installation owns a small
container at the chosen path, switches its `current` link atomically between
versioned payloads, and retains an existing app only through explicit
`source adopt`. The original adopted directory is never modified or removed.

The temporary policy reuses the preparation layer used by `run` and `try`.
For source or AUR builds it creates a private package root, installs only the
requested result and its actual runtime closure, and removes private build
dependencies before the operation returns. Custom external managers must
declare their own `temporary_cleanup` command; unsupported managers are a
strict error or a `--keep-going` skip.

No host package transaction or existing custom application directory was
modified during this verification.

## Verification

- `cargo test`: **441 passed, 0 failed, 2 ignored**.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo build --release`: successful.
- `PACPIN_TEST_BINARY="$PWD/target/release/pacpin" cargo test -- --ignored`:
  **2 passed**.
- `git diff --check`: clean.
- The installed `pacpin` and release binary are byte-identical at SHA-256
  `cd89ffe52bca5f04e94a30b618dfba1f5de23bc5a480f5b6854e2e8535711c94`.
  `pin -> pacpin` remains intact.

The user-selected disposable Garuda live-overlay VM was used with a real Kitty
terminal, 5 GiB RAM for the positive RAM-policy cases, four virtual CPUs, and
the regular guest account. Its relevant versions were kernel `7.2.0-1-garuda`,
pacman `7.1.0.r9.g54d9411-2`, Paru `2.1.0-2.1`, Bubblewrap `0.11.2-1`, and
Kitty `0.48.2-1`. The extra VM memory was temporary and was restored after the
checks.

| Suite | Result | Assertions |
| --- | --- | --- |
| Directory Sources and cleanup | 34 passed | Initial install, atomic update, cancellation, checksum failure, inventory, capture, manifest rebuild, removal preview, unmanaged refusal, explicit adoption, NTFS3-backed publication, hooks, custom external cleanup capability, `--keep-going`, private compiled Source builds, and an actual unprivileged Paru AUR build. |
| `run`/`try` temporary operations | 15 passed | RAM and storage scratch policies, private source build dependencies, prebuilt and native Source execution, controlled exporter errors/escapes/cancellation, output persistence rules, actual AUR build isolation, and a local Git Source build. |

Each suite captured Kitty frames, scrollback, exit status, and before/after
terminal state. Expected negative cases returned their intended nonzero status
without changing the native package database or leaking a workspace.

## Limits

Directory-installed applications do not become ALPM dependents of their native
runtime libraries. Keep a native runtime explicitly with `pin keep PACKAGE` if
normal orphan cleanup could otherwise remove it. The verified publication path
requires the target parent and staged payload to be on the same filesystem.

The raw VM evidence is local and ignored at
`docs/verification/directory-cleanup-evidence/pacpin-directory-final-vm-evidence.tar.gz`.
It contains only fixture output, terminal recordings, and JSON summaries; it
is not published with the repository.
