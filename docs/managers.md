# Manager configuration

Run `pin config init` to create `managers.toml`, `sources.toml`, and `hooks.toml`
beside `config.toml`. Existing files are never overwritten. Setup does this too.
The folder follows XDG_CONFIG_HOME, otherwise `~/.config/pacpin`.
Validate edits with `pin config validate`.

The shipped [template](managers.toml) contains pacman, AUR, Flatpak, Nix and Pipx
entries. Missing `enabled` preserves the legacy `[features]`/`[integrations]`
selection. `enabled = true` explicitly enables an external adapter; false
disables it. Pacman/AUR cannot be disabled or replaced with arbitrary commands:
their adapters retain ALPM planning, lock checks, elevation and unprivileged
builds. Configure `[managers.pacman].privilege_tool` or
`[managers.aur].helper` here; runtime `--sudo`/`--pkexec` still overrides for one run.

Separate recipe/hook files use the same `[sources.alias]` and `[hooks.name]`
tables accepted inline. You can move existing tables there manually. An alias
or hook name defined in both places is an error. Automatic pin changes preserve
these user-owned files. Native `pacman.conf` remains owned by pacman.

## Built-in adapters

| Adapter | Update behavior | Explicit `pin clean` |
| --- | --- | --- |
| Flatpak | Discover installed installation scopes; preserve full refs and exact approved OSTree commits | Remove unused installed runtimes |
| Nix | Modern named flake profile; resolve once per original flake, preview changes, upgrade approved elements with the resolved flake override | Garbage-collect unreferenced store paths; preserve generations |
| Pipx | Look up the environment's actual Python distribution, upgrade only approved environments with version constraints, verify installed versions | No default prune operation |

Flatpak resolves remote-ls' abbreviated commits through remote-info before
approval and checks that the identity still matches. A changing or unresolved
commit makes discovery fail and requires a fresh check.
Flatpak runs with `FLATPAK_FANCY_OUTPUT=0` so its own cursor queries/redraws do
not compete with pacpin's footer or leave terminal replies in the shell input.
Normal messages and authentication remain visible.

Nix-env and unnamed older profile JSON formats require a command adapter;
errors never trigger an automatic switch between profile models. Store-path-only
Nix entries have no upstream to check and are explicitly reported unsupported.
Direct/local/editable Pipx sources likewise require a command adapter; the
built-in PyPI check refuses them instead of guessing a different upstream.
Pipx discovery and installed-version verification use Python's PEP 440 ordering
and normalization, including release candidates, epochs and local versions.
Built-in discovery/verification remains manager-specific even when upgrade or
clean argument arrays are edited. The custom command adapter supports your own
JSON discovery and verification.

Checks must finish successfully before installation. A missing executable,
nonzero exit, malformed/oversized JSON, timeout or failed network query returns
an error and cannot produce a global up-to-date claim. Metadata stdout/stderr
are bounded at 10 MiB and commands at 120 seconds. Supplemental files are limited to
1 MiB, must be regular files, and cannot themselves be symlinks. Script paths
may point to trusted executables. Foreground manager output uses the shared
footer and inherited input; checks use tracked, noninteractive capture.
An optional `env = { KEY = "value" }` table applies consistently to discovery,
upgrade, verification and cleanup. Identity/loader/search-path variables such as
HOME, PATH, TMPDIR and LD_PRELOAD cannot be overridden here. Set TMPDIR through
the scratch flags or invocation environment. Flatpak discovery caching is
disabled for an adapter with environment overrides to avoid mixing inventories.

## Custom managers

```toml
version = 1

[managers.my-manager]
adapter = "command"
enabled = true
check = ["/absolute/path/my-manager-check"]
upgrade = ["my-manager", "upgrade", "--", "{id}", "{version}"]
verify = ["/absolute/path/my-manager-verify", "{id}", "{version}"]
```

The check prints a JSON array of updates:

```json
[
  {"id":"app","name":"App","repo":"main","installed_version":"1","version":"2"}
]
```

`id` and `version` are required. `name` defaults to id; `repo` defaults empty.
Optional `scope` and `revision` carry installation/release identities. Duplicate
id/scope pairs, option-like ids, control characters and unknown fields fail.
An empty array means a successful check with no updates; errors must exit nonzero.
The verifier exits zero only when the approved id/version is installed. This
is a trusted script contract; pacpin cannot independently understand every
custom manager's database or detect unrelated side effects of your script.

Upgrade/verify commands run once per approved package. Placeholders `{id}`,
`{version}`, `{revision}`, `{scope}` expand within literal arguments. Missing
values fail; no shell expansion occurs. Keep `{id}` after the manager's target
separator where supported. Avoid broad upgrade-all commands. Configured scripts
are executable user code: review them and keep them unprivileged. No arbitrary
command is automatically wrapped in sudo/pkexec.

Optional `refresh` runs before discovery on full refreshing upgrades.
Optional `clean` participates only in explicit unscoped `pin clean`.

## RAM, storage and cache flags

```toml
[managers.my-manager]
adapter = "command"
enabled = true
check = ["/absolute/path/my-manager-check"]
verify = ["/absolute/path/my-manager-verify", "{id}", "{version}"]
upgrade = ["my-manager", "upgrade", "--", "{id}", "{version}"]
upgrade_ram = ["my-manager", "upgrade", "--", "{id}", "{version}"]
upgrade_storage = ["my-manager", "upgrade", "--", "{id}", "{version}"]
scratch_mib = 256
no_cache_args = ["--no-cache"]
cache_cleanup = ["my-manager", "clean-cache"]
```

`--use-ram`/`--use-storage` selects the corresponding command variant and supplies
a private TMPDIR on that medium. The shared checks validate the declared
temporary bytes, 128 free inodes, quota and the 2.5 GiB OS RAM reserve.
Forced policies never silently switch medium. Workspace cleanup also runs on
failure/interruption. The manager/script must honor TMPDIR; declaring a variant
does not change daemon policy or redirect the final installation/store.
An incorrect footprint declaration can still exhaust space during execution.

`--no-cache` adds `no_cache_args` before the `--` target separator. If present,
`cache_cleanup` runs after successful work, even when a later manager fails.
It is previewed and its real status is reported. Failed/skipped managers keep
their caches for recovery. With neither mapping, the combination is rejected
before package installation. Pipx has a default cache-disabling mapping.
Flatpak pruning and Nix garbage collection are not silently used for no-cache.

## Custom flags

```toml
[managers.nix.flags.collect-garbage]
command = ["nix-collect-garbage"]

[managers.my-manager.flags.verbose]
args = ["--verbose"]
```

```sh
pin upgrade --external-only --nix.collect-garbage --dry-run
pin upgrade --external-only --my-manager.verbose --use-storage --no-cache
pin manager nix --collect-garbage --dry-run
pin manager nix --collect-garbage
```

Each boolean flag can add upgrade arguments, an action command, or both. Added
arguments go before `--` and affect only that manager. The action runs once
after its successful approved updates, and is part of the approval preview.
No updates means the upgrade flag action is skipped with an explanation.
Use `pin manager NAME --FLAG` to run its command independently; argument-only
flags require an upgrade. Standalone actions preview and ask for confirmation
(or accept explicit `--noconfirm`). They do not claim package upgrades/history.
Unknown flags or flags outside `--aur-only`/`--sources-only` fail. External-only
scope excludes native, AUR and Sources updates. Flags never reach other managers.

## Coverage and limits

This config is for manager operations, not publisher catalogs: use sources.toml
for release recipes and hooks.toml for existing package-update conditions.
General hooks currently watch verified native/AUR/Sources installed **versions**,
not external identities or repository-only changes. Manager flag actions provide
explicit actions after successful external work. Verified external operations
get separate external-upgrade history entries with manager/scope/revision. Native
cache rollback explicitly refuses these records; use the manager's recovery
mechanism. Package manifest rebuild still covers only its documented managers; custom scripts do
not automatically gain rollback, hooks or dependency planning. Pipx dependency
resolution and Flatpak/Nix runtime/store requirements remain manager-owned.

Files are loaded once per invocation. Command templates add parsing/validation
work; scripts and package-manager operations determine their own runtime. Do
not claim this config produces faster upgrades without measurements.
