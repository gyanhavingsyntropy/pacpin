# Security and performance review — 2026-10-13

Reviewed pacpin's privilege and subprocess boundaries, source requests and
credentials, downloads, archive extraction, temporary directories, integration
caches, configuration writes, journal access, hooks, and resolver initialization.
This is a focused code and dependency review, not an exhaustive security audit.

## Findings and fixes

| Finding | Change | Evidence |
| --- | --- | --- |
| Symlink confinement could be bypassed through an intermediate link followed by `..` | Resolve each component in filesystem order; reject absolute targets, root escapes, excessive link traversal, and inspection errors | Previous validator accepted the reproduced escape; new validator rejects it. Unit regressions and real guest source installation cover both malicious and safe chains |
| Flatpak cache writes followed a cache-file symlink and could overwrite its target | Write to an exclusive private temporary file and atomically replace the cache; refuse symlink/special-file reads, check directory ownership, cap reads and sanitize cached display fields | Unit sentinel/FIFO/write-failure checks; controlled Flatpak discovery through the real CLI preserves the sentinel and subsequently uses the cache without another discovery |
| Pipx's PyPI metadata parsing had no body limit and allocated the full release-history JSON tree | Cap the decompressed body at 10 MiB, deserialize only `info.version`, reject unsafe/oversized versions, and warn on rejected metadata | Boundary tests cover oversized bodies, valid-prefix truncation, malformed metadata, Python epoch/local versions and terminal control characters. Guest Pipx fixture uses a real PyPI query |
| Checksum verification depended on an external `sha256sum` process and filename-sensitive textual output | Use the existing SHA-256 dependency with a 64 KiB streaming buffer, cancellation checks, and a regular-file descriptor opened without following a final symlink | Known digest, multiple buffers, newline/option-like filename, symlink, directory, FIFO and missing-file tests; real source checksum success/failure in the VM |
| RustSec flagged a yanked dependency release | Update only `yoke-derive` from 0.8.3 to the compatible 0.8.4 release | Final `cargo audit --json`: zero advisories and zero warnings across 160 locked dependencies |

The symlink reproduction uses `dir/alias -> .` and
`escape -> dir/alias/../../outside`. Lexically this target stays inside the
payload, but Linux resolves it outside after following `alias`. Archive-header
lexical checks and final-component checks alone therefore were insufficient.
The existing libarchive writer safeguards still protect extraction writes;
this fix closes the subsequent link-validation gap before packaging/execution.

The cache issue requires a pre-existing link in the user's own cache directory;
it is an unintended overwrite hazard, not evidence of a remote root escalation.
Checksums verify bytes, not publisher trust. Source-native package scriptlets
and explicitly configured hooks retain their documented trust requirements.

## Measured performance

Release-mode helper microbenchmarks, three repetitions on the host with the
test VM running. These compare the previous operations with captured current
helper implementations. Files were already in page cache. They do not measure
complete upgrades or network time.

| Workload | Previous median | Current median |
| --- | --- | --- |
| Parse the same 7,700,049-byte synthetic PyPI document five times | 403.839 ms | 31.114 ms |
| PyPI parser process peak RSS, including the input fixture | 91,168 KiB | 19,672 KiB |
| Hash a 1 KiB file 200 times | 278.221 ms | 1.033 ms |
| Hash an 8 MiB file ten times | 68.226 ms | 48.358 ms |

On this synthetic large metadata fixture, parsing was about 13 times faster
and process peak RSS was about 78% lower. Small-file hashing benefits primarily
from removing process creation. Large-file checksum throughput still depends
on CPU acceleration, filesystem caching and I/O. The Python benchmark launcher
raises the minimum inherited RSS, so no checksum-memory improvement is claimed.

Raw measurements and the standalone benchmark sources are in
`security-audit-evidence/`. The benchmark builds the exact captured helpers;
its cancellation predicate is false to isolate hashing. Generate fixtures on
a filesystem with space, then run:

```sh
cargo build --release --manifest-path security-audit-evidence/benchmark/Cargo.toml
pacpin-audit-benchmark pypi-old PATH_TO_METADATA_JSON
pacpin-audit-benchmark pypi-new PATH_TO_METADATA_JSON
pacpin-audit-benchmark checksum-old PATH_TO_FILE ITERATIONS
pacpin-audit-benchmark checksum-new PATH_TO_FILE ITERATIONS
```

The metadata fixture has `info.version = "2.0.0"` and 100,000 records under
`releases["1.0"]`, each containing `filename = "unused-release.tar.gz"`,
`size = 12345`, and `upload_time = "2026-01-01"`. JSON uses compact separators.

## Verification

- `cargo test`: **293 passed, zero failed, two ignored**. The explicit real
  makepkg round-trip and signal CLI tests were not individually run as ignored
  Rust tests; guest scenarios independently exercise real packaging/signals.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed.
- Dependency audit database revision:
  `d8325cf4f578e2df4cc5b98a30e9f46b3b88e9f0` (1,309 advisories).
- Guest: disposable Garuda live ISO, VirtualBox, overlay root, regular account
  `garuda`, real Kitty/Xorg terminal, 3,584 MiB RAM. All suites serialized.
- **36 existing CLI scenarios plus four security scenarios passed** on release
  SHA-256 `0883d660969c1b3c11936052d6b271aafdd40dbe5b8c650a34d6192cac070f76`.
  Real pacman/makepkg transactions cover Sources/native/mixed installs,
  narrow/resize/redirected output, checksum rejection, interruption, native
  extraction failure, storage refusal, orphan selection and cleanup failures.
  The new security suite proves rejection without changing the installed source
  version, acceptance of a safe link chain, cache-target preservation and a
  subsequent cache hit. Each CLI case checks locks/workspaces and terminal state.
- Flatpak/Nix/Pipx executable fixtures are controlled adapters, not real app
  upgrades. Pipx discovery queries actual PyPI. Adversarial oversized metadata
  is exercised at the parsing boundary, not through a malicious live HTTPS
  endpoint. AUR builds and password authentication were not rerun in this review.
- One initial security-verifier attempt appended a duplicate TOML integration
  table after source installation had serialized defaults. That fixture error
  was corrected and rerun in a fresh directory; it is excluded from the 40
  passing scenarios. Original attempt evidence remains under `target/maintenance`.
- Evidence exported before shutdown; guest pacman configuration restored,
  no isolated database locks or active pacman/makepkg processes remained.
  VM shutdown confirmed with `VMState="poweroff"` and no running VM entry.
- Installed binary atomically replaced, `cmp` matches the release and `pin`
  still points to `pacpin`. No host packages were installed, removed or upgraded.

The report's small JSON and rendered-screen excerpts are tracked. Full
recordings, frame sequences, logs, initial failure and compressed guest exports
remain in `target/maintenance/security-audit-20261013/`. Tracked screen excerpts
remove trailing whitespace; the original exported recordings are unchanged.

## Remaining opportunities and limits

1. **Incomplete checks need typed outcomes.** Some integration/AUR/network
   errors still become an empty update list. The global up-to-date message can
   therefore be too strong. Rejected PyPI metadata now warns, but the wider
   provider interface still needs explicit incomplete/error results.
2. **Reuse source HTTP agents.** `sources::catalog::request` creates a new agent
   for every request; sharing an agent within the invocation could reuse TLS
   connections across release pages/checksum requests. Preserve HTTPS redirect
   validation and origin-specific token forwarding when changing this.
3. **Avoid repeated configuration subprocesses.** ALPM initialization and cache
   lookup repeatedly invoke `pacman-conf`. A transaction configuration snapshot
   could reduce this overhead, with explicit invalidation after relevant edits.
4. **History queries scan the entire journal.** A reverse bounded reader for
   recent transactions could improve long-lived journal performance. Preserve
   malformed-tail behavior and concurrency rules.
5. **Legacy execution bypasses remain.** Some integration discovery/refresh and
   sandbox provider calls still use raw subprocess APIs instead of shared
   tracking. A separate migration needs interruption/resource tests for each.

RustSec checks Rust dependencies; it does not audit dynamically linked
libarchive/libalpm or prove absence of application flaws. Host versions recorded:
libarchive `3.8.9-1.1`, pacman `7.1.0.r9.g54d9411-4`, Rust `1.94.1`.
No claim about those system packages' current advisories is made here.
