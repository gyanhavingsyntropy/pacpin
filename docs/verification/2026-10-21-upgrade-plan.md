# Stage-aware upgrade execution preview

## Changes

- Approval and dry-run output now share a staged plan from eligible work. Native
  and AUR base commands reuse `pm::build_install_commands`; workspace and display
  details remain runtime concerns. The preview does not assemble one `&&` script.
- Every integration must supply typed `UpgradeAction::Command` / `Internal`
  actions. Configured managers share actual argument expansion with execution;
  invalid plans fail before approval instead of becoming printable commands.
- Sources describe the retained approved recipe/version and its release/Git
  preparation, packaging/build and native archive installation. They no longer
  pretend to execute a recursive `pacpin -S` invocation. Runtime upgrade notices
  show the manager and target count instead of embedding a fabricated command.
- Preparation policy is shown for each manager. Forced workspace policy is
  visible in live previews as well as dry runs. AUR fetching stays helper-owned;
  eligible release downloads may overlap earlier compute/install work, while
  backend-cache preparation stays before its manager's stage.
- The preview describes orphan review after approval, optional/automatic
  selection, revalidated removal only after whole-run success, verified hooks,
  successful-manager cache cleanup and final native/AUR restart inspection.
  Uninvoked managers and inapplicable cleanup stages are omitted.
- History records performed operation descriptions without inventing a shell
  chain around internal actions. Invalid descriptions stop their manager before
  optional preparation or execution. Installation order and cleanup ownership
  remain with their existing coordinators.

## Independent checks completed

- `cargo test`: **398 passed, 0 failed, 2 ignored**.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed.
- `PACPIN_TEST_BINARY="$PWD/target/release/pacpin" cargo test -- --ignored`:
  **2 passed** (real makepkg archive round trip and CLI signal cleanup; no host
  packages installed).
- Release atomically synced to the configured installed binary, byte equality
  confirmed, and the `pin` symlink retained.
- Release SHA-256:
  `2936c9189b477aa1348a693293a6cccffd6393fb7559bd883ed5315124a2144f`.
- VM-verifier Python syntax and `git diff --check`: passed.

New tests cover typed Sources descriptions and stale selection refusal, shared
configured command expansion/no-cache arguments, native/AUR/external stage
scope, separate Sources/Flatpak/Nix rows, conditional lifecycle ordering,
automatic versus interactive orphans, invalid/empty provider actions and narrow
wrapping. The full run also exposed an existing parallel setup-test directory
name collision; adding a per-process atomic sequence isolates those fixtures.

## Rendered VM checks: passed

The user selected the previous disposable Garuda/VirtualBox live guest. Its
first boot stalled while the backing ISO was unreadable; that boot was stopped
without running package fixtures. After the user restored storage access, a
fresh boot completed and the checks below ran as the regular guest user over
SSH using an actual Kitty terminal on the guest Xorg display. Guest root was a
disposable live overlay. Versions: Pacman/libalpm 7.1.0/16.0.1, makepkg 7.1.0,
Kitty 0.48.2, VirtualBox host 7.2.22. The transferred binary matched the release
SHA-256 above. Private VM identity/access details remain in the ignored local
profile and evidence, not this public report.

The saved verifier is `scripts/verify_execution_plan_vm.py`. Transfer it with
the existing upgrade-UI, sequence, capability and preparation verifier scripts,
then run inside the selected disposable guest:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_execution_plan_vm.py /tmp/pacpin \
  --output /tmp/pacpin-execution-plan-review-NEW
```

The verifier completed **9 cases, 0 failures**:

| Case | Observed result |
| --- | --- |
| Mixed native/Sources/orphans, decline | Staged approval shown; installed fixture versions unchanged; no orphan selector opened |
| Mixed dry-run | Same staged review with no installation |
| Narrow native/Sources, storage, no-cache, orphans | One selection Enter; both versions updated; pure orphans removed, optional ones retained; scoped cache cleanup |
| Sources only, no-cache, keep-going | Internal action and Sources progress; no native-repository/AUR stage or irrelevant cache step |
| Native noconfirm without autoremove | Native updated; orphan candidates retained without promising removal |
| Narrow Flatpak/Nix | Separate stages and preparation; each fixture recorded prepare, upgrade, cache in order |
| External dry-run | Both stages shown; neither fixture executed actions |
| Unsupported external storage with keep-going | Only eligible native stage executed and cleaned; Flatpak untouched; exit 2 retained incompleteness |
| No work | No invented execution plan or transaction |

Rendered screen and scrollback captures were inspected at 80 and 44 columns,
including pre-response approval captures, internal Sources text, conditional
lifecycle descriptions and retained native output. The shared verifier checked
terminal restoration, manager row scope, completed Pacman state during Sources,
installed versions and scratch cleanup. A separate narrow live decline was
paused for an actual console screenshot; its prompt/footer were visible and its
verification also passed. Raw captures/results and the screenshot are stored in
the ignored `docs/verification/upgrade-plan-evidence/` directory.

Native/Sources fixtures used real pacman/makepkg; Flatpak/Nix operations were
controlled adapter fixtures, not real application upgrades. This run did not
repeat real AUR compilation, password-required authentication, configured hook
execution, interruption or live resizing; related action/scoping contracts have
unit coverage, and no new end-to-end claim is made for those paths.

Guest pacman configuration was restored and both fixture database locks were
absent before shutdown. The guest was shut down cleanly and VirtualBox confirmed
`VMState="poweroff"`. No host packages were installed, removed or upgraded.
