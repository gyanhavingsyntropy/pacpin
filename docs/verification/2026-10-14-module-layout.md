# Module ownership and folder split — 2026-10-14

## Scope

Baseline: local commit `545782b`. This change separates implementation files and
documents ownership without changing package-manager policy or transaction
ordering. The moved source blocks match their original text, apart from helper
and struct-field visibility required by private child modules. The `cmd_upgrade`
orchestration body is unchanged.

- `pm.rs`: 2,123 → 238 lines. Install, cache, lock and validation implementation
  moved under `pm/`; shared tests moved to `pm/tests.rs`. Existing pm API paths
  remain available through re-exports.
- `commands/upgrade.rs`: 1,835 → 1,128 lines. Explicit target planning moved to
  `commands/upgrade/planning.rs`; tests moved to `commands/upgrade/tests.rs`.
- AGENTS.md now requires reviewing callers, downstream consumers, shared state,
  resource ownership and affected manager/command paths before editing.
- Detailed contracts and VM access instructions moved verbatim (apart from
  heading level) into six documents under docs/development/. AGENTS.md retains
  host/workspace safeguards, review rules, required cargo/binary verification and
  a mandatory index for affected areas. Its size drops from about 35 KiB after
  adding the review rules to about 6 KiB; no original detailed contract was lost.
- docs/architecture.md records the current layout, change-impact checklist,
  known boundary exceptions and staged future divisions. README links to it.
- The four colocated user configuration files remain the supported configuration
  layout. Internal file moves introduce no new user-facing preference.

Line counts include tests and comments. Large progress/sandbox/UI modules and
the remaining long upgrade/install orchestration functions are documented future
targets; this change does not claim to have split them or migrated every legacy
process invocation.

## Rust and structural checks

- `cargo test`: 312 passed, 0 failed, 2 ignored. All previous tests remain
  discovered after moving their modules.
- Explicit ignored checks with `PACPIN_TEST_BINARY=target/release/pacpin`:
  2 passed, 0 failed. Real makepkg packaging and cancellation cleanup are checked
  without installing a host package.
- `cargo clippy --all-targets -- -D warnings`: passed without warnings.
- `cargo build --release`: passed.
- `git diff --check`: passed.
- The release was atomically installed at `$HOME/.local/bin/pacpin`, with
  byte equality checked and the `pin` symlink preserved.
- Host and guest release SHA-256:
  `2658b1c142c9aabf03a3801ee25b7a6ed0fb8b2dc6aafc16a8db78b02b1dccd6`.

Evidence: modularity-evidence (local verification artifact), including Rust logs and
source-movement comparisons.

## Disposable terminal verification

VM: `pacpin-test-vm`, live Garuda overlay under VirtualBox, regular `garuda`
user, 3,584 MiB RAM, actual Kitty X11 display `:0`. Source/native operations use
real makepkg/pacman and isolated fixture databases/caches. AUR checks use real
Paru and the small hello package. Integration command fixtures exercise the
shared orchestration; they do not establish actual Flatpak/Nix/Pipx application
upgrades. Packaging, extraction and cleanup failures are explicitly injected
where named in results.

Reproduction inside the guest, with the existing Kitty socket:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --aur --output /tmp/pacpin-modular-upgrade-20261014
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_housekeeping_vm.py /tmp/pacpin \
  --output /tmp/pacpin-modular-housekeeping-20261014
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --aur --section auth --output /tmp/pacpin-modular-auth-20261014
```

Suites run sequentially because they share the terminal and temporarily own
guest pacman.conf. Rendered screen, scrollback, status, installed versions and
terminal/resource cleanup assertions are retained in compact evidence archives.
The guest configuration and authentication fixture are restored by the verifier.

| Suite | Passed | Failed |
| --- | ---: | ---: |
| Upgrade, including redirected output | 41 | 0 |
| Orphan/cache housekeeping | 21 | 0 |
| Authentication, renewal and real AUR combinations | 11 | 0 |
| Total | 73 | 0 |

The original guest pacman configuration matches its backup, the temporary
authentication policy is absent, and the native database lock is absent.
All evidence was collected before shutdown. The VM is confirmed powered off;
its original 3,584 MiB RAM setting is unchanged.

These checks exercise the affected existing contracts; they are not proof that
all future edits or every package-manager operation will be regression-free.
