# Source build work — completed 2026-10-15

The the VM storage server outage interrupted earlier verification. The user later brought the VM storage server
online and requested completion of the VM checks before a local commit.
Implementation and required verification are now complete.

## Saved implementation

- `sources/build.rs` and private `build/{recipe,profile,tests}.rs` own explicit
  source compilation, trusted local TOML instructions, optimization profiles,
  output inspection and capacity checks.
- `sources/git.rs` resolves release tags before approval and fetches exactly
  the approved commit with restricted protocols/configuration/credentials.
- `execution/resources.rs` owns permission-restricted workspace cleanup without
  following unrelated symlink targets. Shared process tracking remains central.
- `pm/pinning.rs` separates explicit pin intent from helper routing/discovery.
  Resolver VCS policy prevents stale AUR metadata from requesting downgrades.

Defaults inherit makepkg settings; native CPU/optimization level/LTO are opt-in.
See [source-builds.md](../source-builds.md) for the configuration and limits.

## Verified release

Release, installed binary and guest executable SHA-256:
`ee56cb7c35648eec584ce35f00c9dfdda16492d15ce9544d3dfcd24952fb94de`.

- 341 regular tests passed; both ignored real packaging/signal tests explicitly passed.
- Strict Clippy, release build and installed byte equality passed.
- 35 VM scenarios passed: 7 AUR pinning, 13 source build, 4 shared cleanup/progress,
  and 11 authentication/real AUR combinations. The RAM build case verified an
  explicit reserve refusal; a successful RAM compilation is not claimed.
- Real GitHub cmark compiled, installed and ran. Build counters prove successful
  fixture compilations execute once. Moved-tag, failure, cancellation, unrelated
  symlink, prompt visibility, cache scope and lock cleanup checks passed.
- Guest configuration restored, test authentication policy removed, locks clear,
  evidence exported, and clean VM poweroff confirmed. No VM remains running.

Reports and durable evidence:

- [Source compilation and final VM verification](../verification/2026-10-15-source-builds.md)
- [AUR pinning and VCS policy](../verification/2026-10-15-aur-pinning.md)
- [Architecture and affected-module map](../architecture.md)

No required source-build verification remains pending. Broader features such as
arbitrary branch tracking, automatic build-system inference, upstream VCS commit
discovery for AUR, and PGO/BOLT automation were not implemented or claimed.
