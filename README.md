# pacpin

**Declarative Package Resolver & Upgrade Engine for Arch Linux / CachyOS**

> [!WARNING]
> **Experimental: no stable release has been published yet.** Bugs are possible.
> Pacpin has not
> completed a release-grade security review. Avoid unattended upgrades and
> running untrusted packages with `pin try` or `pin run`; test in a disposable
> VM, inspect each proposed transaction, and keep system backups.

`pacpin` is a high-performance native package resolver, system upgrade engine, and package sandboxing tool written in Rust for Arch Linux and CachyOS. It combines declarative repository pinning, repo-level exclusions, stability delay buffers, companion split-package cascades, vendor stickiness, multi-package manager unification, post-upgrade restart inspection, ephemeral sandboxing, and transaction snapshot rollback journaling directly on top of `libalpm` via zero-copy C FFI.

---

## Key Features

1. **Declarative Repository Pins**: Lock packages or wildcard patterns (`linux-firmware*`, `amd-ucode`) to specific repositories (`core`, `extra`, `cachyos`, `aur`). Prevents blind upgrades from epoch-polluted or experimental third-party repos.
2. **True Resolver Engine**: Synthesizes explicit qualified package targets (`repo/pkg`) instead of blind `pacman -Su` commands, preventing unexpected repository hopping.
3. **Opt-In Vendor Stickiness**: Keep packages bound to their originating repository (`%INSTALLED_DB%`) during upgrades so they don't unexpectedly jump between sync repositories (e.g. `core` ➔ `cachyos`), displaying a distinct `[sticky]` badge in transactions.
4. **BIOS-Style Repository Priority Menu**: Interactively reorder your system's repository search priority using arrow keys and instant promotion/demotion (`pacpin repos`).
5. **Ephemeral Package Sandbox (`pacpin try [repo/]pkg [args...]`)**: Download and execute native Arch packages in a Bubblewrap (`bwrap`) container with unshared namespaces, read-only system mounts, private in-memory `/tmp`, SHA256 verification, and archive traversal guards. Flatpak apps use Flatpak's own sandbox; Nix and Pipx execute on the host only with explicit `pin run` or `--no-sandbox`.
6. **Post-Upgrade Restart Inspector (`needrestart`)**: Automatically inspect running processes holding deleted `.so` libraries in RAM (`/proc/*/maps`) and verify running kernel vs installed modules (supporting Arch, CachyOS, XanMod, RT, TKG, and Bochs kernels), advising on exact service restart commands (`sudo systemctl restart <srv>`) with zero noise when nothing needs restarting.
7. **Scrollable Checkbox TUI with Batch Shortcuts**: Interactive viewport checklist (`Space` toggle, `Enter` confirm) with one-key batch actions (`[a]` All, `[n]` None, `[p]` Pure Only, `[i]` Invert, `/` Search) for orphan management and setup.
8. **Configurable Package Manager Integrations**: Check Flatpak, modern flake-backed Nix profiles, Pipx, and custom managers alongside Pacman/AUR. Preview exact scoped targets, stop on failed checks/transactions, and explicitly declare manager-specific flags and cleanup actions in [managers.toml](docs/managers.md).
9. **Batch Pinning from Files**: Pin multiple packages at once or import a package list from a text file (`pacpin pin <repo> -f <file>`).
10. **Stability Delay Buffers**: Enforce quarantine windows (e.g. `pacpin delay linux 3`) with automatic reverse-dependency cascade holds to prevent ABI mismatches.
11. **Companion Cascade Detection**: Automatically detects split packages (sharing `%BASE%`) and prefix-related dependencies when pinning or installing to prevent version desynchronization.
12. **Transaction Snapshots & Rollback**: Logs transaction state to `~/.local/state/pacpin/history.jsonl` and enables 1-click package rollback (`pacpin rollback`) directly from the local pacman cache.
13. **Single-Confirmation**: Prompts once before execution; passes `--noconfirm` to low-level runners (`pacman` and `paru`).
14. **Upstream Sources (experimental)**: GitHub, GitLab, HTTPS catalogs, and local/company catalogs share declarative recipes and the same verified native-install pipeline. Recipes can package prebuilt releases or compile verified source archives/release-tagged Git unprivileged. Optimization inherits makepkg by default; CPU, level and LTO overrides are opt-in per recipe in sources.toml. Use `pin -S alias/package`; [recipes and trust limits](docs/sources.md), [source compilation and optimization](docs/source-builds.md).

---

## Configuration (`~/.config/pacpin/config.toml`)

All editable files live together under `$XDG_CONFIG_HOME/pacpin`, or
`~/.config/pacpin` when XDG_CONFIG_HOME is unset:

| File | Purpose |
| --- | --- |
| `config.toml` | Pins, package manifest, repository order and general preferences |
| `managers.toml` | Manager adapters, literal commands, supported scratch/cache mappings and custom flags |
| `sources.toml` | Optional upstream release recipes using `[sources.alias]` |
| `hooks.toml` | Optional post-update actions using `[hooks.name]` |

`pin config init` creates missing manager/recipe/hook files with documented
defaults. Setup also creates them. Existing files are preserved, and read-only
checks never create them. `pin config validate` checks all files. Existing inline
`[sources]` and `[hooks]` remain supported; duplicate aliases/hook names across
files are errors. Automatic pin updates leave separate recipe/hook files intact.
See [manager configuration and flags](docs/managers.md), [release recipes](docs/sources.md),
and [post-update hooks](docs/verification/2026-10-11-runtime-hooks.md).

```toml
repo_order = ["core", "cachyos", "extra", "multilib"]

[features]
pinning = true
vendor_stickiness = false
stability_delays = false
smart_orphans = true
integrations = false
shell_aliases = true

[options]
helper = "paru"
# Used by `pin clean` and its AUR helper's pacman invocation.
privilege_tool = "sudo" # sudo, doas, or pkexec

[pins]
"amd-ucode" = "core"
"intel-ucode" = "core"
"linux-firmware*" = "core"

[exclude]
cachyos = ["linux-firmware*"]

[delay]
# "linux" = 3
# "openssl" = 3

[integrations]
flatpak = true
nix = false
pipx = true

[packages]
pacman = ["core/pacman", "extra/git", "flatpak"]
aur = ["paru-bin"]
flatpak_user = ["flathub/org.gnome.Calculator"]
flatpak_system = []
```

`pin capture --dry-run` previews a package list based on explicitly installed packages; `pin capture` saves it to `[packages]` (use `--force` to replace an existing list). `pin rebuild --dry-run` previews a rebuild, and `pin rebuild` installs only missing entries. Existing packages absent from the list are never removed. An already-installed package is not reinstalled by default; `pin rebuild --enforce-repos` explicitly reinstalls repo-qualified entries from their listed repositories. Use `--noconfirm` only after reviewing the plan.

Pacman's local database does not reliably record which repository originally supplied a package. Capture uses a matching pin or a unique current sync-repository match as a best-effort source guess, not proof of original provenance; review all entries, especially unqualified and foreign packages, before using the list elsewhere. Configure the same repositories, an AUR helper, and Flatpak remotes on the new machine first. This rebuild restores package presence, not versions, services, system settings, or dotfiles. Nix and Pipx are not yet part of `[packages]`.

---

## Commands Reference

### Core Package Operations
| Command | Pacman / AUR Equivalent | Description |
| :--- | :--- | :--- |
| `pacpin check` (`pin check`, `pin -Qu`) | `pacman -Qu` | Check pending updates and verify active pin protections |
| `pacpin upgrade` (`pin -Syu`, `pin up`) | `pacman -Syu` | Safe system upgrade with True Resolver (`-y` refresh, `-n` dry-run, `-c` clean orphans). Explicit `pin -Syu <pkg...>` targets join the upgrade in their own box, and approval covers the solver-expanded plan (new dependencies, removals) |
| `pacpin -S [repo/]pkg` (`pin install`) | `pacman -S`, `paru -S` | Install package(s); auto-pins `repo/pkg` with companion cascade (`-y` refresh, `-n` dry-run) |
| `pacpin -Sp <pkg...>` (`pin -Sp`) | `pacman -Sp` | Natively resolve and print package and missing dependency download URIs via ALPM |
| `pacpin -Sl [repo...]` (`pin -Sl`) | `pacman -Sl` | List packages in sync repositories with installation status |
| `pacpin -Sg [group...]` (`pin -Sg`) | `pacman -Sg` | List package groups or packages in a designated group |
| `pacpin remove <pkg...>` (`pin rm`, `pin -Rns`) | `pacman -Rns` | Remove package(s) and unneeded dependencies with **Smart Unpin** cleanup |
| `pacpin search <query...>` (`pin -Ss`) | `paru -Ss`, `pacman -Ss` | Search official repositories and the AUR simultaneously |
| `pacpin info <pkg...>` (`pin -Si`) | `paru -Si`, `pacman -Si` | View package metadata, dependencies, and upstream repository info |
| `pacpin clean` (`pin -Sc`) | `pacman -Sc`, `paru -Sc` | Clean cached package archives and AUR build directories |

After you confirm an interactive upgrade with sudo, pacpin can download repository packages into pacman's cache while the orphan selector is open. The shared footer shows this download; the selector preserves its complete inventory in scrollback and supports page navigation, Home/End, and one Enter to confirm. pacpin waits for that download-only operation to finish before installing. With pkexec or doas, repository downloads stay in the normal foreground operation. If prefetch fails, the normal upgrade downloads the packages. Selected orphans are removed only after a successful upgrade and a fresh check of the installed package graph.

Sources release downloads can overlap a recognized verification, build or install
phase of an earlier manager. AUR fetching remains owned by Paru, which has no AUR
download-only mode. Its footer shows the current file or Git transfer percentage,
then changes to building/installing; that percentage is not the whole AUR upgrade.
Helpers may fetch later inputs after starting a build, so overlap is opportunistic.
Git source compilation stays in its foreground stage.

With automatic scratch policy and its canonical upgrade command, Flatpak prepares
the exact approved ref/commit using download-only mode immediately before its
upgrade. This avoids concurrent backend-cache writes without a shared space budget.
Nix, Pipx and custom managers can opt in with a literal `prepare = [...]` command
in `managers.toml`; it must download only and include `{id}`. Forced scratch omits
this optional preparation, and cache-avoidance upgrades omit it unless cleanup is
declared. The actual upgrade still obeys its validated flags. Preparation failures
retry normally; unsupported optional preparation never skips an eligible upgrade.
Custom wrappers may report `PACPIN_PROGRESS download DONE/TOTAL` and
`PACPIN_PROGRESS phase=build|verify|install` for concise footer updates.

Confirmed orphan cleanup is recorded separately from the known-package inventory. If an upgrade fails or removal cannot finish, a later full upgrade offers cleanup again, even with no package updates and an unchanged known list. Stored selections are a reminder, not permission to remove packages without a new review. Declining the new review stops repeated prompts for that unchanged list. Read-only orphan listings, scoped upgrades and `--noconfirm` without explicit autoremove preserve pending cleanup. Older state gets one fresh review when this tracking is first introduced. `pin orphans -c` also resolves pending cleanup.

Removal checks the entire selection against the installed dependency graph. Dependencies needed by a retained package, including an unchecked optional orphan, are kept with an explanation. Removal is limited to the confirmed targets; unchecked dependencies remain installed. Orphan removal and cache cleanup appear as separate footer steps; completed upgrades remain checked while cleanup runs. `--keep-going` does not enable orphan removal after an upgrade failure.

The shared footer describes repository refresh, checks, review, installation and cleanup. Horizontal phase tabs show pending, active, completed or failed work. Completed updates keep a tick while Orphans or Cache becomes active. Pending cleanup lives only in the tabs; the rows below show current work, such as Pacman download progress or native/AUR/Flatpak/Nix cache cleanup. Approved AUR and external upgrades stay separately **Pending** while earlier package managers run, and completed managers remain checked during later package upgrades. Failed/skipped work retains its diagnostics and cannot earn an overall update tick. Narrow terminals prioritize active tabs and count omitted tabs; short terminals prioritize active work and recent failures.

Enabled external managers (including Sources, Flatpak and Nix) check for updates concurrently. Each waits only for its own required metadata refresh: Sources can check while Flatpak and native databases refresh, and native/AUR resolution follows native refresh. Individual Sources packages also check concurrently, using up to four workers across aliases; a free worker starts the next queued package immediately. The combined plan waits for the selected checks to finish. Package transactions retain their ordered execution. Native target lookup prefers an exact package name across repositories before virtual providers; explicit repository targets remain confined to that repository. Dependency-preview failures identify conflicting packages or unsatisfied dependencies.

With `--keep-going`, a failed source package check excludes only that exact `alias/package`. Other packages, including those sharing the alias, remain eligible. Counts for failed checks remain unknown and the invocation returns incomplete (exit 2); the footer warns about skipped checks while retaining healthy Sources progress. Without the flag, any failed check stops before installation; read-only `pin check` also requires complete discovery.

Ordinary native install dry-runs also prepare the transaction and show solver-added dependencies and removals. Full system transactions delegated directly to pacman state when dependencies have not been expanded. Forced native reinstalls remain included in capacity estimates.

Upgrade approval shows short, unnumbered action labels, for example
`Actions: Pacman · Sources · Orphan review · Cache cleanup (on success)`.
These labels do not imply sequential downloads. `--dry-run` additionally shows
base manager commands, internal Sources actions and per-manager preparation;
runtime paths/options are added during execution. No combined shell script is
invented for internal or conditional work. Dry runs perform none of these actions.

Orphan selection follows approval, and selected removal requires revalidation
and whole-run success. Hooks require verified changes; cache cleanup applies to
successful invoked managers. Restart advice follows cleanup without adding an
approval-plan action. Scheduling installation for the next restart is currently
not implemented; `needrestart` reports restart needs after updates.

The final plan summary follows dependency and removal details. Its native counts and sizes include the prepared transaction's additional packages: for example, `41 upgrades + 7 new dependencies`, with no `+ 0` when there are none. Sources and other managers retain unknown sizes rather than appearing to add zero bytes.

**Package Space Delta** covers installed package files only. With native
`--no-cache` work, **Pacman Cache Now** separately estimates logical file bytes
in configured caches before downloads. It is `Unknown` for nested/unreadable
contents or a large scan; other managers' cache sizes are not guessed. This is
not guaranteed reclaimable disk space: snapshots, shared extents and later
downloads change the result. Cache cleanup after success does not reduce peak
installation space.

`--no-cache` cleans native/AUR caches involved in successful manager transactions.
Sources remove temporary workspaces separately. Other managers must declare
cache-disabling arguments or a cache-cleanup command; unsupported combinations
fail before package installation. Pipx defaults to `--no-cache-dir`. Removing
installed Flatpak runtimes or old Nix generations is a separate explicit action,
not automatic cache cleanup. Successful managers can clean their caches even if
a later manager fails; failed-manager caches are retained. Cleanup failures make
the command fail, and cleanup finishes before restart advice.

Full native cleanup also clears inactive ALPM `download-*` directories inside configured native caches, including sandbox-owned partial downloads. The database must be unlocked and nonempty directories must be at least two minutes old. Recent nonempty downloads and partial (`-Sc`) cleanup preserve their contents and report incomplete cleanup. Symlink targets and other filesystems are preserved; AUR-only cleanup never scans native caches.

Use `pin clean -Scc --repo` for native caches or `pin clean -Scc --aur` for AUR caches. Both scopes exclude enabled integration cleanups, including in dry runs. The two selectors cannot be combined; AUR cache cleanup requires an available helper.

### Choose upgrade managers and temporary storage

Native/AUR installs create repository pins only for explicit `repo/package`
targets and companions you select for that repository. An unqualified
`pin -S package` that falls back to AUR does not create a pin, and dependencies
discovered by Paru do not inherit one. Existing exact or wildcard pins still
apply. Sources retain their separate ownership rules.

For AUR VCS packages (standard `-git`, `-svn`, `-hg`, `-bzr`, `-darcs`, `-cvs`,
or `-fossil` suffix in the package name or base), older advertised metadata does
not request a downgrade, even with an AUR pin: the built version is determined
by `pkgver()`. Newer advertised versions can still be offered. This does not
implement upstream commit discovery when the advertised version is unchanged.

```sh
pin upgrade --sources-only                 # configured upstream Sources only
pin upgrade --aur-only                     # AUR targets only
pin upgrade --external-only                # enabled Flatpak/Nix/Pipx/custom managers
pin upgrade --sources-only --use-ram       # Sources download / packaging in tmpfs
pin upgrade --aur-only --use-ram            # Paru clones / downloads / builds in tmpfs
TMPDIR=/path/to/disk/scratch pin upgrade --sources-only --use-storage
pin -S aur/antigravity --use-ram            # one AUR package
pin -S source-alias/package --use-storage   # one Source package
pin -Syu --keep-going                      # continue eligible managers; report skipped work
pin run --use-ram extra/jq -- . input.json  # downloads + extracted files in RAM; host execution
pin try --use-storage extra/jq -- . input.json # disk files; Bubblewrap isolation
pin run --dry-run --use-ram extra/jq        # inspect the workspace policy without downloading
pin rebuild --use-storage --no-cache --dry-run # preview declared native/AUR/Sources work
```

`--sources-only`, `--aur-only` and `--external-only` are mutually exclusive. Each excludes unrelated manager upgrades and native repository refresh, including when spelled `-Syu`; AUR helpers can still install required build dependencies through pacman. Keep the native system current before AUR updates. Scoped runs do not remove orphans automatically; use `pin orphans -c` separately.

`--use-ram` and `--use-storage` are mutually exclusive, invocation-only preferences for Sources/Paru temporary workspaces and native repository `run`/`try` files. Defaults retain automatic Sources allocation and the helper's existing AUR workspace; native `run`/`try` checks `TMPDIR` then `/var/tmp`. Forced RAM requires tmpfs, quotas/inodes and the 2.5 GiB OS reserve; it never silently falls back to disk. Forced storage requires a writable non-tmpfs filesystem, using `TMPDIR` or `/var/tmp`. No mount or filesystem repair is performed. AUR build footprints are unknown: the starting capacity check cannot guarantee a large compilation will fit. Tmpfs can also be swapped by Linux. These flags do not move already-installed files, native pacman installation downloads, helper metadata, or installed build dependencies out of root. Root installation capacity remains required. Manifest `rebuild` forwards scratch/cache policy to native/AUR/Sources installation; a manifest Flatpak installation rejects these overrides before changing packages.

Paru receives a private clone directory and makepkg directory environment overrides. Successful temporary workspaces are removed; failed AUR workspaces are retained with their path for inspection (RAM contents disappear on reboot). Custom clone/makepkg/chroot/local-repository flags conflict with this policy; other helpers fail explicitly rather than ignoring the request. Sources retain their automatic temporary-workspace cleanup.

`--keep-going` skips a manager whose metadata refresh or update check failed, and managers whose requested upgrade scratch/cache options are explicitly unsupported. Successful check results are retained. Failed discovery is shown before approval with an unknown update count; it is never reported as up to date. Eligible managers continue, while the result stays incomplete (exit 2), even when none have updates. Orphan cleanup stays pending. Hooks still use verified successful changes; an unrelated check failure does not suppress a script, while AUR rebuild hooks are skipped when AUR is ineligible. Skipped managers perform no upgrade, prefetch, namespaced custom action or cache cleanup; successful invoked managers can still clean their own caches. Invalid local configuration, insufficient capacity, native dependency/refresh failures and uncertain installation failures remain fatal. Read-only `pin check` still requires complete discovery.

Runtime continuation is opt-in and limited to recognized makepkg build failures under the default native/dependency policy with Paru. It requires no observed native installation, an unchanged pacman database and log, an unlocked database, and satisfied installed dependencies. AUR build-dependency installations therefore conservatively disable continuation. Unknown failures, interruptions and native extraction/commit failures stop the run. Only Sources opt in: each exact approved package is checked independently, including transitive installed dependencies, actual archive dependencies, a prepared ALPM transaction with no extra packages/removals, and root/scratch capacity. Packages with scriptlets, conflicts or replacements are excluded. Successful Sources are recorded individually; the AUR failure still produces a nonzero exit, skips orphan cleanup and preserves failed AUR caches. This is not a recovery command for damaged packages.

For an eligible continuation run, Paru uses `/usr/bin/pacman` and `/usr/bin/makepkg` without chroot or local-repository installation. Custom forwarded helper flags disable continuation. Any attempted Sources installation without verified completion blocks later packages even if the pacman log has not changed.

Cache removal after installation does not eliminate temporary installation space. pacpin checks estimated native root/cache capacity before downloading and again before installing; source builds also check their final installation destination. Downloads, build scratch, final net growth, and temporary rewrite space are separate requirements. These estimates cannot guarantee space for every snapshot, quota, concurrent writer, or package hook.

### Post-update hooks

Configure actions in `hooks.toml` (existing inline hooks in `config.toml` also work)
for `pin upgrade`, `pin -Su`, or `pin -Syu`:

```toml
[hooks.rounded-blur]
watch = ["mutter", "gnome-shell"]
action = "rebuild-aur"
packages = ["gnome-rounded-blur"]

[hooks.refresh-user-data]
watch = ["some-installed-package"]
action = "script"
command = ["/home/you/.local/bin/refresh-user-data", "--refresh"]
```

Hooks appear before the final plan summary, including in dry runs. Each matching
hook runs once after successful transactions and verified installed version
changes, before orphan/cache cleanup and restart advice. Several watched updates
still cause one execution. New installs, unchanged versions, failed transactions,
and unverified updates do not trigger hooks. Hook failure makes the run fail and
skips remaining hooks; already installed updates are not rolled back.

Conditions combine: `watch = ["pkg-a", "pkg-b"]` means **a OR b**;
`watch_all = ["pkg-a", "pkg-b"]` means **a AND b**;
`unless_updated = ["pkg-c"]` vetoes the hook if **c** also changed version.
Omit `watch` when using only `watch_all`. If both positive lists are present,
at least one `watch` package and every `watch_all` package must change. An alternative
to the first rounded-blur hook is:

```toml
[hooks.rounded-blur]
watch = ["mutter", "gnome-shell"]
unless_updated = ["gnome-rounded-blur"]
action = "rebuild-aur"
packages = ["gnome-rounded-blur"]
```

This runs after either GNOME package changes, unless rounded-blur was also updated.
Conditions are checked against the approved plan and again against installed
versions after successful transactions. Hook actions do not trigger further hooks.

Script arguments are passed literally, without an implicit shell. Use an absolute
executable path. Scripts run as the invoking regular user with `/` as their working
directory; `PACPIN_HOOK` names the hook and `PACPIN_UPDATED_PACKAGES` contains the
verified watched package names separated by spaces. Nested pacpin invocations
inheriting `PACPIN_HOOK` skip hooks to prevent recursion. Scripts own their effects;
pacpin does not infer package installs or cache ownership from script output.
Configure only scripts you trust. Hooks watch approved native, AUR and Sources
package updates; Flatpak/Nix/Pipx IDs are not triggers. Hooks are not run by ordinary
`pin -S`, recovery, or package managers invoked independently of pacpin.

AUR rebuild hooks currently support Paru and already installed foreign packages
only. They force rebuilding the current AUR recipe even when its version is
unchanged, reuse privilege/scratch settings and participate in AUR `--no-cache`
cleanup. A failed rebuild preserves its cache. `--sources-only` skips AUR rebuild
hooks and explains why; script hooks can still watch Sources packages.

The [Blur My Shell upstream guide](https://github.com/aunetx/blur-my-shell/blob/master/scripts/GUIDE.md)
requires rebuilding rounded-blur after GNOME Shell/Mutter updates and logging out
after installation. The library recipe must also support the new GNOME version;
the guide mentions a compatibility patch for GNOME 51. A hook cannot make an
incompatible upstream recipe build successfully.

### Declarative Rules & Configuration
| Command | Description |
| :--- | :--- |
| `pacpin repos` (`pin priority`) | Interactive BIOS-style repository search priority menu |
| `pacpin pin <repo> <pkg...>` | Lock package(s) or wildcards to a designated repository |
| `pacpin pin <repo> -f <file>` | Batch pin packages from a text file (one package per line) |
| `pacpin unpin <pkg>` | Remove a repository pin rule |
| `pacpin delay <pkg> <days>` | Set a stability delay buffer on a package |
| `pacpin undelay <pkg>` | Remove a stability delay buffer |
| `pacpin list` | List active engine features, pins, exclusions, delays, and repo priority |
| `pacpin reset [-f]` | Reset all configurations, pins, and delays to default (creates automatic timestamped backup) |
| `pacpin init [--reset]` (`setup`) | Guided current preferences, manager selection and configuration review; backs up the previous main file before saving |
| `pacpin config init` | Create missing editable manager, source and hook files without overwriting existing files |
| `pacpin config validate` | Validate the main config and all supplemental files |
| `pacpin manager NAME --FLAG [-n|--noconfirm]` | Preview or run configured standalone manager actions |
| `pacpin capture [-n|--force]` | Save explicitly installed packages and Flatpak apps to `[packages]` (`-n` previews) |
| `pacpin rebuild [-n|--noconfirm] [--enforce-repos]` | Install missing listed packages without deleting extras; optionally reinstall from declared repositories |

#### Guided setup

Run `pacpin init` as your regular user in a terminal. It covers privilege/AUR
tools, repository policies and delays, orphan review, cache preferences and
Flatpak/Nix/Pipx participation. It also explains Sources build recipes, conditional
hooks, declarative packages, temporary apps, manager capabilities and per-command
scratch/scope controls. It does not install packages or execute recipes/hooks.

Existing values are the defaults when reconfiguring. Press Enter to keep a value,
or type `q` to cancel; EOF also cancels. Invalid answers are retried. A final review
requires an explicit **yes** before writing. Setup never writes shell aliases
before that approval, and new setups leave alias maintenance disabled by default.
Existing `config.toml` is backed up as `config.toml.backup-*.toml` beside the main
file. Edits to any of the four configuration files while the wizard is open
prevent saving, including comment-only edits.

All editable configuration lives in the same directory (normally
`~/.config/pacpin/`, or `$XDG_CONFIG_HOME/pacpin/`):

| File | Purpose during setup |
| --- | --- |
| `config.toml` | Preferences, repository rules and `[packages]` declarations |
| `managers.toml` | Built-in/custom adapters, explicit manager selection, capabilities and custom flags |
| `sources.toml` | Release/catalog recipes and trusted local source-build commands/optimization settings |
| `hooks.toml` | Conditional commands or AUR rebuilds after verified changes |

Missing supplemental templates are created after saving; existing files are
preserved byte for byte. Explicit manager `enabled`, `helper` or `privilege_tool`
overrides are shown with their owning file rather than offered as ineffective
main-config edits. Use `pacpin config init` to create only missing supplemental
templates without interactive setup, and `pacpin config validate` to check them.

`pacpin init --reset` starts main preferences, rules and package declarations
from defaults, then asks for approval and backs up the old main file (even if its
TOML is malformed). It retains supplemental definitions; fix malformed
supplemental files before running setup. `init --help` describes the flow.
Runtime flags such as `--pkexec`, `--use-ram`, `--no-cache` and `--keep-going` belong
to operations; setup rejects them. Choose a persistent privilege tool and cache
preference in the wizard instead. Forced scratch placement is never a universal
default for external managers with different storage models.

### System Maintenance & Hygiene
| Command | Description |
| :--- | :--- |
| `pacpin orphans [-c]` (`pin autoremove`) | Inspect or remove unneeded orphaned dependencies with scrollable interactive checkbox TUI |
| `pacpin keep <pkg...>` (`pin adopt`) | Mark package(s) as explicitly installed to silence orphan warnings |
| `pacpin needrestart` (`pin restart-check`) | Inspect processes holding outdated libraries (`/proc/*/maps`) or replaced kernel in RAM |

### Power Tools & Sandboxing
| Command | Description |
| :--- | :--- |
| `pacpin try [--no-sandbox] [repo/]pkg [args...]` (`pin run`) | Run native Arch packages in an ephemeral Bubblewrap container (`--no-sandbox` / `--bare` runs on the host; host execution cannot remap absolute data paths such as `/usr/share` fonts, so data-heavy apps should use sandboxed `try`) |
| `pacpin history [id]` | View transaction history timeline or inspect full package diff |
| `pacpin rollback [id] [-n]` | Restore previous package versions from pacman cache |

> **Sandbox Security Note**:
> - **Installed packages without a repository**: An unqualified native `try`/`run` target uses its installed package-owned executable when no configured repository offers it. `try` retains Bubblewrap isolation; `run` executes on the host. `--bin` must belong to that package. Explicit `repo/package` targets still require that repository. No automatic AUR build is attempted.
> - **Choosing a version**: `pin try --installed PACKAGE` and `pin run --installed PACKAGE` explicitly choose the local version. Without this flag, an available repository candidate is downloaded for testing, including a newer version, and the installed version is reported and preserved. Use `repo/package` to select a particular repository. `--installed` cannot be combined with a repository/provider prefix.
> - **Temporary storage**: Native repository `run`/`try` keeps new archives and extracted files together in the selected workspace and removes them on exit or handled interruption. It checks live quota, inode and memory capacity before download, then checks actual archive paths and sizes before extraction. These are checks, not reservations; the running app can consume more memory. The footer reports aggregate download bytes/percentage, checking capacity and extraction, then retires before app execution. Redirected output uses milestones without cursor controls.
> - **Flag composition**: Native `run`/`try --no-cache` bypasses existing archive reuse; it does not purge the system's pacman cache. `--dry-run` previews without starting the app. Put application flags after `--`, including flags named `--use-ram`, `--no-cache` or `--pkexec`. An installed-only target rejects forced scratch placement because its files stay installed. Flatpak/Nix/Pipx `run`/`try` rejects scratch overrides rather than claiming to relocate their stores. Flatpak/Nix also rejects `--no-cache`; Pipx uses a private environment and download-cache workspace which is removed on exit, preserving normal home data and installed Pipx environments. Its 64 MiB starting check cannot predict arbitrary Python build footprints. See [command and flag roles](docs/cli-controls.md).
> - **Application output**: `run` preserves normal project/home writes. `--use-ram` relocates temporary package files only; generated files still use their normal filesystem. Native `try` discards its private home and `/tmp`, and makes an eligible current project read-only; `--rw` preserves writes to that mounted project. The home tree, its ancestors and system paths are excluded from project mounts even with `--rw`. See [output persistence and Java examples](docs/cli-controls.md#application-files-and-persistence).
> - **Executable selection**: `--bin` selects an entry point owned by the requested package, including toolkit `bin`/`sbin` directories under `/usr/lib` or `/opt`. It cannot select a downloaded dependency's tool. For a JDK use `pin run --use-ram --bin java jdk-openjdk -- Hello` or `--bin javac` to compile.
> - **Credential & Home Isolation**: Sandboxed containers run in unshared Linux user, mount, IPC, PID, and network namespaces with read-only root filesystems and a private in-memory tmpfs over `$HOME`. The host home tree is excluded from current-directory mounts to keep host credentials (`~/.ssh`, `~/.gnupg`, tokens) isolated.
> - **Hardware & GUI Passthrough**: Flags like `--gui` and `--audio` pass through host display sockets (`/tmp/.X11-unix`, Wayland) and audio nodes (`/dev/snd`), which intentionally broadens the trust boundary for desktop interaction.
> - **Archive & Input Verification**: Ephemeral packages are downloaded with fail-closed SHA256 integrity verification, streamed through structured `tar` header validation, and guarded against symlink/hardlink directory escapes.

### Transparent Pacman Drop-in
`pacpin` serves as a complete drop-in wrapper. Any native pacman flag sequence (`-Syu`, `-S -y -u`, `-Ss`, `-Si`, `-Sc`, `-Sp`, `-Sl`, `-Sg`, `-Sw`, `-T`, `-Q`, `-Qi`, `-Ql`, `-Qo`, `-F`, `-Fy`, `-U`, `-D`, etc.) passed to `pacpin` or `pin` is transparently handled with proper permission routing (non-root for queries, `sudo` for modifications).

---

## Development architecture

See [module ownership, affected-module checks and the refactoring plan](docs/architecture.md)
and [design principles with professional references](docs/development/design.md)
for where features belong and which related paths to review. [AGENTS.md](AGENTS.md)
records the development and verification contracts.
The [orphan-menu handoff audit](docs/verification/2026-10-15-orphan-handoff.md)
records the sudo-rs terminal fix and its transaction, authentication and footer checks.

## Building from Source

### Prerequisites
Building `pacpin` requires `pacman` / `libalpm`, `libarchive` (system libraries and headers), `pkgconf`, and a modern Rust toolchain (Rust 1.85+, Edition 2024 required by `alpm 5.0.2`). `bubblewrap` is required at runtime for container sandboxing. Package extraction links to the system libarchive and uses its native tar/gzip/xz/zstd/bzip2 support; keep this system dependency updated. Separate decompressor commands are no longer used for extraction.

```bash
# On Arch Linux / CachyOS
sudo pacman -S --needed base-devel git rust pacman libarchive pkgconf
```

Bubblewrap is optional until you use native `pin try` or `pin run --sandbox`.
It is not installed by `pin init`: on first use, pacpin offers to install it
through its normal package-install workflow and verifies that isolation works.
Without an interactive terminal (or with `--noconfirm`), a missing Bubblewrap
causes a clear error; preinstall it with `pin install bubblewrap`. Pacpin never
silently falls back to unsandboxed execution. Libarchive is a required shared
library, not an on-demand tool; Arch's pacman already depends on it.

### Build & Install
```bash
cargo build --release
install -m 755 target/release/pacpin ~/.local/bin/pacpin
install -m 755 target/release/pacpin ~/.local/bin/pin
```

---

## License

`pacpin` is licensed under the **GNU General Public License version 3 (GPLv3)**. See the [LICENSE](LICENSE) file for the full license text.

```
Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>
License GPLv3+: GNU GPL version 3 or later <https://gnu.org/licenses/gpl.html>
This is free software: you are free to change and redistribute it.
There is NO WARRANTY, to the extent permitted by law.
```
