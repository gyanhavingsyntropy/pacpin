# Footer prompts and aggregate native progress — 2026-10-13

## Reproduction in the disposable VM

The Garuda live guest was started with 3584 MiB RAM and its real Kitty terminal
(80×30 initially). The existing native/source fixtures used real pacman and
makepkg, with an isolated guest database/cache. Only guest pacman required the
disposable account's password; the verifier restored that guest policy and
pacman configuration after the baseline suite.

The authentication verifier was changed to wait two seconds before answering
and inspect Kitty's rendered screen. The prior test typed as soon as it found
the prompt in a raw recording, missing this regression.

Baseline command inside the guest:

```sh
PACPIN_DISPOSABLE_VM=1 python /tmp/verify_upgrade_ui_vm.py /tmp/pacpin \
  --section auth --output /tmp/pacpin-prompt-baseline-auth-3
```

All five successful transaction scenarios lost their password prompt before
input: native only, Sources only, native plus Sources (two consecutive runs),
native plus Sources with cache cleanup, and ticket renewal. Each still installed
its fixture successfully after the test entered the guest password. Native-only
also lost `PACPIN_NATIVE_HISTORY_01` from screen/scrollback. The sixth scenario,
an injected sudo-session preparation failure, failed safely as expected.

The terminal trace showed buffered footer setup arriving after sudo's direct
`/dev/tty` prompt. Repeated creation/resizing inserted padding and moved normal
output to the bottom. A supplemental local Kitty screen-engine fixture then
exposed another failure: shrinking the window could move an unanswered prompt
into scrollback while bottom-anchored footer rows remained visible.

## Changes

- Complete and flush footer setup before children start. Preserve the normal
  cursor and scrollback without adding newline padding on each phase or teardown.
- Defer footer reservation during initial authorization and suspend it when an
  observed authorization phase awaits secret input. Resume from actual child
  output; sudo's subsequent raw-mode relay still receives progress updates.
  Password input remains owned by the child on the inherited controlling tty.
- Native download progress uses byte weights from uncached archives in the
  approved transaction, or pacman's aggregate `Total` bar. Cache overrides are
  respected. An ambiguous truncated archive label suppresses inferred totals.
  Background native output uses a private output PTY so pacman emits bars while
  the orphan selector retains input ownership.
- Package installation, verification and removal percentages use the full
  package counter and current package progress. For example, package 29 of 49
  at 100% is 59% of that installation stage. Narrow footers retain the percent.
- The VM verifier now checks prompt visibility after waiting, with standard
  sudo prompt text and resizing during authentication in its native-only case.
  AGENTS.md records these contracts.

## Completed local verification

Required tests, strict Clippy and the release build used Cargo from
`$HOME/.cargo/bin`, with test scratch on the repository HDD:

```sh
PATH="$HOME/.cargo/bin:$PATH" cargo test
PATH="$HOME/.cargo/bin:$PATH" cargo clippy --all-targets -- -D warnings
PATH="$HOME/.cargo/bin:$PATH" cargo build --release
```

Final results: **288 passed, 0 failed, 2 existing explicit-only tests ignored**.
Strict Clippy passed. Unit checks cover weighted parallel downloads, cached and
partial archives, aggregate bars, ambiguous/truncated names, large sizes,
installation counters, narrow percentages and phase padding.

The controlled PTY test also waits through authentication, resizes, checks no
footer cursor movement before input, verifies the fixture secret is not echoed,
and confirms continued output rendering during an echo-disabled child relay.

`scripts/verify_footer_screen.py` runs that controlled Rust fixture through the
installed Kitty screen engine without a display or package transactions. Its
explicit checks confirmed the waiting prompt remains visible after shrinking to
40×18, and earlier native/source output remains in screen/scrollback. This is a
supplemental parser check, not an actual guest CLI or password-policy test.
Kitty's embedded Python disables assert statements, so this script raises
explicit exceptions for every verification condition.

Example command (use the current Cargo unit-test executable and a fresh output):

```sh
kitty +runpy 'import runpy, sys; sys.argv = ["scripts/verify_footer_screen.py", "--binary", "target/debug/deps/pacpin-3e00fc85950ff7df", "--output", "target/maintenance/prompt-new"]; runpy.run_path(sys.argv[0], run_name="__main__")'
```

Build logs and supplemental screens are in
`target/maintenance/prompt-20261012/`. Compact engine results and screens are
retained alongside this report.
Committed screen excerpts trim trailing whitespace and empty final rows; the
complete screen captures and raw stream remain in the local evidence directory.

The release was atomically synced to `$HOME/.local/bin/pacpin`, checked with
`cmp`, and the `pin -> pacpin` symlink preserved.

## Pending VM verification

The fixed release was transferred and its authentication suite started. The
host subsequently restarted and the VM storage server is offline again. Those follow-up guest
results were not exported and are **not counted as verified**. Baseline guest
screen files were also lost with the ephemeral live guest; the reproduction
above records the observed tool output and inspected terminal trace.

The complete authentication, native/source/AUR, orphan, narrow-screen,
scrollback, cancellation and cleanup suites need to run again with the VM storage server available,
sequentially, with evidence exported before shutdown. No test VM process remains
after the host restart; a graceful guest shutdown for this run is not claimed.
