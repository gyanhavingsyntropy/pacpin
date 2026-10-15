# pacpin development instructions

## Preserve the user's workspace and host

- Preserve intentional uncommitted changes. Do not commit, push, or change tags unless explicitly requested.
- The host root filesystem is small and often nearly full. Avoid large scratch files there; inspect disk space and available RAM before VM work or package builds.
- Do not install, remove, or upgrade host packages for verification. Use dry runs on the host and actual transactions only in a disposable VM.
- Start pacpin unprivileged. Run makepkg and AUR helpers as the regular user. Elevate only native pacman operations. Use the user's selected privilege tool; this user's administrative preference is pkexec.
- Source compilation requires an explicit local TOML build recipe. Inherit makepkg optimizations by default; CPU, level and LTO overrides are opt-in. Resolve Git before approval, never acquire build commands from remote metadata, and verify build/output failures cannot reach installation. Build code is unprivileged, not sandboxed. Test failed makepkg directory cleanup and preserve unrelated symlink targets.
- Shut down a disposable VM started for verification after the checks and confirm it has stopped, so it does not retain RAM.

## Review affected modules before editing

Read [the design contract and professional references](docs/development/design.md) before changing shared APIs.

- Read [the architecture and change-impact map](docs/architecture.md) before changing a shared contract. Locate the owner, its callers and the downstream consumers with `rg`; inspect the implementation and relevant tests in each affected module. The map is a starting point, not an exhaustive dependency graph.
- Before editing, state the behavior being preserved or changed and the affected command/manager paths. Follow data as well as function calls: option parsing, approved targets, verified results, progress state, authentication, resource ownership, persistent state and cleanup can cross several modules.
- A shared implementation does not establish identical manager capabilities. Check native, AUR, Sources and enabled integrations individually for any changed flags, return types, error handling, completion or cleanup. Review scoped, no-work, failure, interruption, dry-run and non-TTY paths when applicable.
- Derive footer stages from eligible approved work. Show pending AUR and each external manager before native downloads start; named discovery is separate from installation success. Preserve actual active/completed/failed/skipped state, bound rows in small terminals, and verify rendered prompts and scrollback in the VM as specified by the linked UI contract.
- A refactor must preserve argument ordering, approval boundaries, privilege ownership, process tracking, terminal state, locks, workspaces and partial-failure reporting. Moving a function does not authorize replacing its tracked executor with a raw spawn or widening the resources it may delete.
- Keep module ownership explicit. Command handlers coordinate typed operations; adapters translate manager-specific operations; shared infrastructure owns execution and resources. New external interactions go through the existing owner. Record legacy exceptions and migrate them deliberately instead of copying their pattern.
- Split by responsibility and resource lifetime. Keep a small facade with narrow exports so callers need not know internal file layout. Prefer explicit imports and `pub(super)`/`pub(crate)` over wildcard imports or unnecessarily public helpers. Related private modules belong in the owner's directory; do not add a generic `helpers` module to conceal unrelated work.
- Review a production module around 600 lines or a function around 100 lines for mixed responsibilities. These are review prompts, not hard quotas: tests, FFI and a cohesive state machine may justify a larger file. Dense one-line code still needs review. Avoid both giant orchestration functions and tiny files that force constant navigation.
- Make structural moves separately from behavior changes. First preserve bodies and contracts, then improve the implementation in a distinct reviewed change. Keep focused tests with their owner and shared contract fixtures in a clearly named test module.
- Update docs/architecture.md when ownership, exports, caller relationships or verification requirements change. Keep user preferences in the existing colocated config files unless a new file has a distinct user-facing responsibility; internal file splits do not require new configuration.
- Run the required verification and binary sync below after Rust changes, and the affected disposable-VM scenarios after execution/UI/orchestration changes. State any unverified paths explicitly. Documentation and a passing build are not proof that a transaction releases its lock or that a terminal prompt is visible.

## Required reading for the affected area

These linked contracts are development instructions, not optional background.
Before editing, read every document applicable to the affected behavior, including
callers and downstream consumers identified by the architecture map. Structural
moves still require the contracts of the code being moved.

| Area being changed or exercised | Required instructions |
| --- | --- |
| Shared API, ownership or module layout | [Design contract](docs/development/design.md) and [ownership map](docs/architecture.md) |
| Manager/configuration/flag behavior, integrations, sources or scratch/cache policies | [Manager scope and capabilities](docs/development/managers.md) |
| Process, privilege, network, archive, filesystem or persistence boundaries | [External interaction ownership](docs/development/boundaries.md) |
| Transactions, planning, selection, cleanup, progress, terminal input/output or failure handling | [Transaction and UI contracts](docs/development/transactions-ui.md) |
| Metadata/input validation, archive handling, caches, sandboxing or resource safety | [Security and resources](docs/development/security.md) |
| Any Rust change and its release verification | [Verification scenarios and evidence](docs/development/verification.md) |
| Starting, accessing, testing in, or stopping the disposable guest | [Garuda VM access](docs/development/vm.md) |

Keep this index and [docs/architecture.md](docs/architecture.md) current when
ownership changes. Preserve detailed instructions in the referenced document
when reorganizing them; moving a rule does not remove it or weaken its scope.

## Required verification after code changes

Use `PATH="$HOME/.cargo/bin:$PATH"` for Rust commands. Run:

1. `cargo test`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo build --release`
4. Sync the release to `$HOME/.local/bin/pacpin` and verify byte equality. Preserve the `pin` symlink. Prefer atomic replacement; do not leave a failed staging file.

For terminal, progress, scheduling, or manager orchestration changes, also verify the actual CLI in the disposable Garuda VM using a real terminal emulator. Unit tests or PTY recordings alone do not establish that the screen is correct. Check rendered screen and scrollback as well as command logs and installed versions.
