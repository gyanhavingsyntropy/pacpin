# Unified cache estimates and temporary-workspace messages

## Changes

- The summary uses one **Cache Now** row for eligible planned managers under
  `--no-cache`. Native estimates still use bounded, read-only inspection of
  configured Pacman cache roots. AUR helper and external cache sizes remain
  explicitly unknown; their locations are not guessed. Known bytes are retained
  as `~size + unknown`, with the unknown managers named below the card.
- Each integration must declare its cache-usage contract. Sources own temporary
  artifacts rather than a retained package cache. Unplanned/skipped managers do
  not enter inspection, and measurement never runs cleanup commands or changes
  cleanup scope. Installed Flatpak runtimes and the Nix store are not counted as
  disposable caches. Package Space Delta remains separate from these estimates.
- Replaced the archive-box icon: Kitty gives the old character one cell while
  pacpin counted two. The folder icon has a predictable two-cell presentation.
- An unused forced workspace policy now names `--use-ram` or `--use-storage`
  and explains that no temporary build files are needed. The message also
  distinguishes installed disk files from temporary workspace placement.

README, AGENTS.md, ownership documentation and transaction/UI contracts were
updated. Version remains **3.3.0-alpha.1**. No dependencies, flags or user
configuration files were added.

## Verification

- `cargo test`: **412 passed, 0 failed, 2 ignored**.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed.
- Both explicit ignored release tests passed: makepkg archive round trip and
  signal/workspace cleanup. They performed no host package installation.
- Python verifier syntax and `git diff --check`: passed.
- Release atomically synced to the installed binary; byte equality checked and
  the `pin` symlink preserved. SHA-256:
  `85c83aaf3c194bc084a5b2c52572f275a1c1b5cc4b6d3be9d349dbebda868f95`.

Used the user-selected disposable Garuda/VirtualBox live-overlay guest, regular
guest execution over SSH and actual Kitty/Xorg screen/scrollback captures.
Guest and host release checksums matched. Guest tools: Pacman/makepkg 7.1.0,
libalpm 16.0.1, Kitty 0.48.2 and classic sudo 1.9.17p2.

**19 scenarios passed:** 11 review cases, six extra cache/cleanup cases and two
fresh-authentication sequences. Coverage includes:

- Native-only RAM/storage flags, declined approvals, mixed native/Sources,
  Sources-only, controlled Flatpak/Nix, scoped/skipped/no-work paths and dry runs.
- Captured 80/44-column cards checked independently with Kitty's width engine:
  all eight border/content rows measured **64/42 cells**, respectively. This
  checks rendered output independently of pacpin's width helper. The verifier
  now retains this regression assertion.
- A 4 KiB native cache plus unknown external caches; incomplete native inspection
  retaining a known subtotal and naming Pacman as unknown. Dry runs preserved
  the fixture files, including nested contents.
- Cache-disabling arguments without a cleanup command, real native/Sources
  updates with orphan removal/cache cleanup during resize, and redirected output.
- One visible password prompt reused through a native/Sources/cleanup run, then
  one fresh prompt on the next invocation despite an existing terminal ticket.

Native/Sources used real pacman/makepkg; external actions were controlled
adapters, not actual Flatpak/Nix application upgrades. AUR summary behavior has
unit coverage; real AUR compilation and sudo-rs were not repeated in this change.
Exact AUR/external cache measurement remains unavailable, rather than reporting
an invented total or reclaimed-space promise.

Raw captures and command logs are local in the ignored
`docs/verification/cache-summary-evidence/` folder. Guest pacman configuration
and authentication policy were restored, and fixture database locks were absent
before shutdown. No host package was installed, removed or upgraded. No GitHub
push or tag changes were performed. The guest shut down cleanly; VirtualBox
confirmed `VMState="poweroff"`.
