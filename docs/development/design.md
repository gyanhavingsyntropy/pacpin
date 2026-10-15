# Design rules and their rationale

This is the design contract for shared pacpin APIs. Read it with the ownership
map in [architecture.md](../architecture.md); the map records actual code, while
this document explains how to choose boundaries for new work.

## Professional references

| Reference | Principle | Application in pacpin |
| --- | --- | --- |
| John Ousterhout, [Designing Abstractions](https://web.stanford.edu/~ouster/CS349W/lectures/abstraction.html), and [A Philosophy of Software Design](https://web.stanford.edu/~ouster/cgi-bin/aposd.php) | A small interface should hide substantial complexity; adding more interfaces can also add complexity. | Own process/resource lifetimes behind execution APIs. Split mixed responsibilities; retain cohesive state machines rather than imposing a maximum function length. |
| Alistair Cockburn, [original Ports and Adapters article](https://alistair.cockburn.us/hexagonal-architecture) | Separate application decisions from UI and external technology through replaceable adapters. | Upgrade policy consumes discovered inventory and adapter capabilities; manager adapters own concrete commands, identities and verification. Tests can replace adapters without pretending fixture commands are real manager upgrades. |
| [Rust API Guidelines: dependability](https://rust-lang.github.io/api-guidelines/dependability.html) and [type safety](https://rust-lang.github.io/api-guidelines/type-safety.html) | Prefer types that enforce valid inputs; validate properties that cannot be expressed statically. | Required capability methods and distinct unsupported/invalid errors prevent silent default support and message-based continuation. Live capacity and installed-result checks remain runtime checks. |
| [Rust Book: module privacy](https://doc.rust-lang.org/book/ch07-03-paths-for-referring-to-an-item-in-the-module-tree.html) | Hide implementation details and deliberately expose interfaces. | Keep owner facades and private implementation modules; use pub(super) for sibling implementation needs and narrow crate exports. |

These are design guidance, not an instruction to reproduce a framework or create
one crate per layer. pacpin remains one binary crate with responsibility-based
modules. A documentation rule alone cannot enforce a boundary.

## Rules for new and changed features

1. **Name one owner for each decision and resource.** Specify inputs, outcomes,
   callers and lifetime before choosing files. CLI parses intent; orchestration
   orders approved work; adapters interpret manager policy; execution tracks
   children/cancellation; resource guards own their temporary paths; the UI
   presents facts supplied by those owners.
2. **Make shared requirements part of the interface.** A manager must implement
   `IntegrationProvider::check_options`. It must explicitly accept a policy or
   return `OptionError::Unsupported` / `OptionError::Invalid`. Do not add a
   default success implementation. Match typed variants, not diagnostic text. Core AUR and external stages use
   distinct identities; a display name must not determine which target set is
   excluded.
3. **Use one eligibility result.** The upgrade preflight projects discovered
   work before counts, approval, hook planning, prefetch or execution. Skipped
   managers cannot enter downloads, commands or cache cleanup through a second
   interpretation of the original inventory. Never silently relax forced flags.
4. **Keep capability gaps distinct from runtime failure.** With --keep-going,
   a declared unsupported upgrade option may exclude its manager before effects.
   Invalid configuration, bad metadata and insufficient capacity remain fatal.
   Runtime native/unknown failures remain fatal; verified AUR build failure has
   its separate guarded Sources continuation contract. An incomplete plan keeps
   a nonzero result and preserves pending orphan cleanup.
5. **Decisions should be testable without launching a program.** Keep typed
   capability classification, target projection and progress arithmetic separate
   from process/terminal ownership. Do not let a progress percentage establish
   transaction success. Verify adapter output and installed identities through
   the owning boundary.
6. **Keep dependencies pointing toward owners.** Shared execution does not
   import sandbox or progress. The terminal layer registers a restore callback
   for cancellation during session startup; execution can run without a UI.
   Archive/database inventory shares executable-path policy through utils,
   rather than depending on app preparation.
7. **Use explicit state and guards where they remove ambiguity.** Do not encode
   unsupported/failed/completed as the same boolean. Keep approval data separate
   from actual installed results. Registered resources and child guards retain
   cleanup on error. A destructor cannot substitute for an observable operation
   result, and process::exit paths need their explicit cleanup contract.
8. **Isolate test state.** Resource-registry tests use their own registry instance;
   they must not drain the invocation registry shared by other parallel tests.
   Exercise owned cleanup and unregister behavior without deleting another
   test's workspace.
9. **Test the contract across its consumers.** Unit tests cover typed policy
   decisions; adapter/CLI fixtures cover effects and argument boundaries; the
   disposable VM covers real pacman/makepkg, locks, authentication, interruption
   and rendered terminal state. Include no-work, scope, dry-run and failure
   cases. State which external managers used fixtures.

## Current enforcement and remaining limits

Rust enforces private child modules and requires every provider to implement the
capability method. The preflight unit tests cover typed error handling and
inventory projection. Existing process/capture tests were moved with their
owner; VM suites exercise actual operation and terminal lifetimes.

The binary crate does not enforce every dependency direction: crate-visible
APIs can still be called incorrectly. Upgrade/install orchestration still has
long functions and legacy forwarding/removal paths remain recorded in the map.
No universal approved/executed/verified typestate has been implemented. Introduce
such state types when migrating those paths; do not claim a folder split has
already prevented every bypass or made every future feature consistent.
