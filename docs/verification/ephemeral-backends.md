# Temporary application backend verification

Verified on 2026-10-23 for the development changes after the published
`v3.3.0-alpha.1` tag. The existing tag was not moved. The tested release and
installed binary both have SHA-256:

```text
49ebb090b841d37098ac6020909ee1b11d7ed9250f8d9f2f363cb1d7b4acf93e
```

## Rust checks

- `cargo test`: 423 passed, 2 explicitly ignored.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed.
- `PACPIN_TEST_BINARY="$PWD/target/release/pacpin" cargo test -- --ignored`:
  both the real makepkg archive and signal/workspace cleanup tests passed.
- Release atomically installed and byte-compared; the `pin` symlink preserved.

## Disposable guest checks

Used the user-selected Garuda live guest in VirtualBox, as its regular user.
Tools: Pacman `7.1.0.r9.g54d9411-2`, Paru `2.1.0-2.1`, Bubblewrap `0.11.2-1`,
Kitty `0.48.2-1`. Actual Kitty screen and scrollback captures accompany command
logs; these checks were not limited to PTY output.

| Suite | Scenarios | Result |
| --- | ---: | --- |
| Temporary AUR/Sources/custom exports | 14 | Passed |
| Git-input Sources compilation | 1 | Passed |
| Existing native/installed run/try regressions | 34 | Passed |

The temporary suite compiled a Sources package using a missing repository build
dependency extracted privately. Neither the dependency's ALPM entry nor its tool
appeared in the guest system. Compiled, prebuilt-archive and native-package
Sources paths passed; the Git case used an exact tagged local Git fixture.
The real AUR case fetched, built and ran GNU Hello through Paru recipe fetching
and makepkg, without installing it. Package/configuration state fingerprints
were unchanged by temporary operations.

Custom export fixtures covered RAM placement, normal run output persistence,
try's private home, dry runs, command failure, escaping links and Ctrl-C (130).
Owned workspaces disappeared after success, failure and interruption. Compiler
checks retained a build/preparation label; they did not imply verification or
installation. Footer teardown preserved the surrounding terminal text.

Native regressions covered installed-only/fallback execution, explicit repository
failure, package-owned executable selection, newer candidates preserving the
installed version, output persistence/isolation, scoped download progress,
RAM/storage/cache flags, application arguments after `--` and unsupported options.
The harness now accepts successful RAM execution only with a tmpfs app path,
or the explicit OS-reserve refusal when the selected guest lacks headroom.

The initial 3584 MiB guest correctly refused a RAM export when the required
2.5 GiB OS reserve could not be retained. Successful RAM cases used a temporary
5120 MiB allocation. The guest was shut down and its original allocation restored
after verification. No host packages were installed or removed.

## Limits

Custom exports were fixtures, not proof of every third-party manager's export
implementation. Built-in Flatpak/Nix/Pipx storage contracts were retained; those
stores do not become disposable RAM workspaces. Git-input compilation used a
local repository; the temporary GitHub network path was not separately exercised.
The real AUR test was Hello, not every AUR dependency/provider graph. Ambiguous
AUR providers/cycles fail explicitly. Installation hooks/services are not run
for temporary packages, and successful examples do not establish that all
applications work without system installation.

Raw recordings remain in the ignored `ephemeral-backends-evidence` directory.
