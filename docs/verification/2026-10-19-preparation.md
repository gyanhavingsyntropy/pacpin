# Download preparation and AUR footer verification — 2026-10-19

## Changes

- `progress/transfers.rs` interprets curl, wget and Git transfer output for AUR,
  and scoped Flatpak/custom progress. Percentages describe the current transfer;
  they never establish installed success or a whole AUR transaction total.
- `progress/observer.rs` consumes fragmented charset designation/reset sequences
  such as makepkg's `ESC(B`. Previously the designation's `B` could leak into
  normalized text and hide an exact build/failure marker.
- Build/configure diagnostics retain the declared build/package phase. An
  optional feature check, "password" in compiler output, or staged file install
  must not impersonate verification, sudo authentication or a system install.
  ALPM counters/hash bars retain installation/verification arithmetic rather
  than becoming AUR file downloads. Narrow footers preserve percentages.
- `integrations/preparation.rs` owns approved optional jobs and their lifetime.
  Each provider must declare Background, BeforeStage or Unsupported. Earlier
  recognized verification/build phases can start later release-artifact jobs;
  launching Paru or finishing one file cannot release that gate. Jobs join
  before the owning manager executes; aborted plans cannot start waiting work.
- Builtin canonical Flatpak upgrades can download the exact approved ref/commit
  before deployment, with `--no-deploy --no-deps --no-related --noninteractive`.
  Backend caches stay immediately before their own stage because no shared
  cross-manager storage reservation exists. Nix/Pipx/custom managers can declare
  a literal target-scoped `prepare` command in `managers.toml`.
- Forced scratch omits optional configured preparation. Cache-avoidance upgrades
  omit preparation cache writes unless cleanup is declared. Actual upgrades keep
  their validated options. Unsupported optional preparation does not skip an
  otherwise eligible upgrade. Preparation failure allows the ordinary stage to
  retry; failure warnings do not promise a retry when finalizing an aborted plan.
- Background command observation uses neutral bounded/tracked execution capture;
  workers cannot write into another manager's terminal display. Completion ticks
  still require actual successful upgrades. README, manager template, AGENTS and
  ownership/UI/verification contracts document the new boundary.

Paru's upstream [release notes](https://github.com/Morganamilo/paru/releases)
disable AUR download-only installation; `paru -G` only retrieves PKGBUILD-related
files, as documented in its [README](https://github.com/Morganamilo/paru#examples).
The [Flatpak command reference](https://docs.flatpak.org/en/latest/flatpak-command-reference.html)
documents download-only update options. No AUR compilation is disguised as
download preparation. Helpers may fetch later inputs after a first build phase:
overlap is opportunistic, not exclusive bandwidth scheduling across every package.

## Verification

- `cargo test`: **377 passed, 0 failed, 2 ignored**.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo build --release`: successful.
- `PACPIN_TEST_BINARY="$PWD/target/release/pacpin" cargo test -- --ignored`:
  **2 passed** (actual makepkg archive round trip and signal cleanup).
- `git diff --check`: clean.
- **20 distinct disposable-VM scenarios passed**; eight external scenarios were
  repeated after the final warning-wording change (**28 passing executions**).

The VM used Garuda's live overlay under VirtualBox, 3584 MiB RAM/four CPUs,
regular user `garuda`, real Kitty screen/scrollback, interactive shell job
control, and 80/40-column terminals. Native and Sources fixtures performed real
pacman/makepkg transactions. Real AUR cases used Paru and the upstream `hello`
PKGBUILD (2.12.1-2), private makepkg source/package paths and curl `--limit-rate
96k` to expose intermediate received-byte percentages. Review was disabled only
in the disposable test account. No host package transaction was performed.

| Group | Passing scenarios |
| --- | --- |
| Real AUR (4) | AUR only; AUR+Sources; native+AUR+Sources; ordinary `pin -S aur/hello` |
| External preparation (8) | Separate Flatpak/Nix rows; narrow no-cache; failed preparation followed by successful upgrade; dry-run; forced storage; unsupported optional preparation; no work; skipped manager with successful native work |
| Native/Sources (8) | Sources only; native+Sources; resize/no-cache; source preparation interruption; native only; ordinary native install; fatal native download failure without Sources/cache effects; colored AUR build-failure continuation |

Assertions cover installed versions, intermediate AUR percentages, Sources
remaining pending during initial fetching in AUR+Sources, Sources downloading during actual
AUR compilation, completed/pending manager rows, retained native hook/history
text, literal approved preparation arguments, regular-user execution, operation
order, dry-run/no-work/skipped effects, cache scope, restored terminal settings,
and no leaked Source workspace or ALPM lock. Negative scenarios intentionally
return nonzero. The colored build-failure helper is controlled; its subsequent
guarded Sources packaging/installation is real and the overall result stays failed.

External adapters in these suites are **controlled literal-command fixtures**,
not actual Flatpak/Nix/Pipx application upgrades. Builtin Flatpak argv/ref/commit
constraints and unsupported policy variants also have Rust unit coverage. Git
progress has parser coverage; no large real Git clone or Nix daemon/store was
exercised. Passwordless guest sudo does not establish password-prompt/ticket
behavior; long orphan-review/authentication matrices were not rerun for this change.

The first real-terminal pass exposed curl's `MM:SS` time format (in addition to
`HH:MM:SS`) and makepkg's charset-reset marker issue. Both received regression
coverage. Reruns also exposed old `hello` fixture files left behind when earlier
suites restored a different ALPM database. Those fixtures were removed through
their own guest databases; the verifier now removes its real AUR package before
restoring configuration. Those failed/reproduction runs are not counted above.

## Reproduction and evidence

Transfer `verify_preparation_vm.py`, `verify_upgrade_sequences_vm.py`,
`verify_capabilities_vm.py`, `verify_upgrade_ui_vm.py` and the release to guest
`/tmp`, using [the VM access instructions](../development/vm.md), then run serially:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_preparation_vm.py /tmp/pacpin \
  --output /tmp/pacpin-preparation-aur-new --section aur
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_preparation_vm.py /tmp/pacpin \
  --output /tmp/pacpin-preparation-core-new --section core
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_preparation_vm.py /tmp/pacpin \
  --output /tmp/pacpin-preparation-external-new --section external
```

Use fresh output directories. The external section repeats the eight focused
adapter scenarios from core. The shared verifier lock prevents concurrent suites.

Evidence is in preparation-evidence (local verification artifact): actual console PNGs,
readable terminal samples and result/binary JSON. `scenarios.tar.gz` retains all
rendered frames, scrollback, exit statuses and before/after terminal settings.

The 20-case matrix used release SHA-256
`45d0e91ce1e40a77765b96478386560b44f427a1acee61d4821f59d1e95a92db`.
The final release only changes two preparation warning strings, removing a promise
of a retry during aborted-plan finalization. All eight external cases were rerun
with final SHA-256
`a534673ef085edb74f998e4816b6d99a64379a205cdeb450b654227fe7c9767d`.
The installed `$HOME/.local/bin/pacpin` is byte-identical to that release;
`pin -> pacpin` is preserved. No commit, push or tag was created.

VM shutdown confirmed after evidence export: `VMState="poweroff"`.
