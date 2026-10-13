# Command and flag roles

Pacpin controls have an operation-specific meaning. They are not automatically
forwarded to every backend. Application arguments belong after `--`.

| Commands | Scratch flags | Cache control | Preview / other relevant controls |
| --- | --- | --- | --- |
| `upgrade`, `-Syu`, `-Su` | Sources/Paru; external managers need declared variants and footprint | Successful invoked managers only; capability checked before work | `--dry-run`, scopes, `--keep-going`, confirmation, configured manager flags |
| `install`, `-S` | Sources/Paru temporary files; native installation retains pacman policy | Invoked native/AUR transaction only; Sources files are already ephemeral | `--dry-run`, `--channel`, refresh, confirmation |
| `rebuild` | Same typed native/AUR/Sources installation boundary | `--no-cache` / `--clean-cache` forwarded to installation | `--dry-run`, `--enforce-repos`, confirmation; manifest Flatpak rejects scratch/cache overrides before work |
| Native repository `run`, `try` | New archives and extracted files share RAM/disk workspace | `--no-cache` bypasses archive reuse and leaves existing system cache intact | `--dry-run`, `--bin`, isolation options; app flags after `--` |
| Installed native `run`, `try` | Rejected: existing installed files cannot be relocated by scratch policy | No package download; existing caches untouched | `--installed`, `--bin`, `--dry-run`, isolation options |
| Flatpak/Nix/Pipx `run`, `try` | Rejected: these execution adapters do not declare a store/environment relocation capability | Flatpak/Nix reject; Pipx uses an owned temporary environment/cache workspace and cleans it on exit | `--dry-run`; Nix/Pipx require host execution opt-in; Flatpak keeps its own sandbox |
| `manager NAME --FLAG` | Upgrade scratch flags are irrelevant to standalone actions | Define the explicit cache action in managers.toml | `--dry-run`, confirmation; configured literal commands have separate approval |
| `clean` | Rejected: removes existing caches rather than building packages | Its own partial/full and native/AUR scope controls | `--dry-run`, confirmation; integration pruning is distinct from transaction cache removal |
| `rollback` | Rejected: uses installed system paths and native archives | Preserves rollback archives; `--no-cache` rejected | `--dry-run`, `--allow-partial`, confirmation; external history is not native rollback |
| `remove`, `rm`, `-R`; `orphans`, `autoremove`; `keep`, `adopt` | Rejected: no temporary package build | `--no-cache` rejected; use explicit `clean` separately | Existing removal/selection/confirmation controls; `keep --noconfirm` is an explicit no-op because marking never prompts |
| `check`, search/info/query/files/dependency checks, history, restart advisory | No scratch build; rejected | `--no-cache` rejected | Read-only inspection; explicit native repository/files refresh can require elevation |
| `capture`, pins, repo order, delays, reset/init, config commands | No package workspace; rejected | `--no-cache` rejected | Their own file/force/preview controls; `capture --dry-run` previews, `config validate` reads, `config init` creates missing supplemental files |

`--sources-only`, `--aur-only` and `--external-only` apply to upgrades, exclude
unrelated checks/refresh/cleanup, and are mutually exclusive. `--keep-going`
retains the verified AUR build-failure-to-Sources contract; it is not a general
ignore-errors switch. Namespaced flags apply to their configured manager's
upgrade or separately approved standalone actions.

`--sudo` and `--pkexec` select the privilege boundary for operations which need
it. They are mutually exclusive, do not elevate package builds, do not consume
another option's value, and leave arguments after `--` unchanged.

## Application files and persistence

Scratch placement and application output are separate policies:

| Native execution mode | Project files | Home/settings | App temporary files |
| --- | --- | --- | --- |
| `run` | Normal host writes persist | Normal host writes persist | App/host policy; not relocated by `--use-ram` |
| `try` | Mounted project is read-only | Private tmpfs; discarded on exit | Private `/tmp` tmpfs; discarded on exit |
| `try --rw` | Writes to mounted current project persist | Private tmpfs; discarded on exit | Private `/tmp` tmpfs; discarded on exit |

`--use-ram` puts downloaded native archives and extracted package files in
tmpfs, and removes that workspace when the app exits. It does not redirect Java
output, project builds or user settings into RAM. Such output still requires
space wherever the app saves it. A later run prepares the package again; it is
not a permanent installation. Sandboxing flags change the execution mode:
`run --sandbox` uses try's isolation, while `try --no-sandbox` uses host execution.

Try only mounts eligible project directories: the home tree, its ancestors and
system directories are excluded, even with `--rw`. It uses private `/tmp` when
the current directory is excluded. Run has no such directory restriction.

For a JDK with multiple tools, choose the entry point explicitly:

```sh
pin run --use-ram --bin javac jdk-openjdk -- Hello.java
pin run --use-ram --bin java jdk-openjdk -- Hello
```

The first command preserves `Hello.class` in the current project. Native
entry points come from the requested package's inventory, including its
`usr/bin`, `usr/sbin` and toolkit `bin`/`sbin` directories beneath `usr/lib` or
`opt`. Downloaded dependencies cannot supply the selected entry point.

## Scratch defaults and live checks

Sources chooses RAM when its estimate fits with a 2.5 GiB OS reserve, otherwise
disk. AUR retains the helper's configured build location without a forced flag.
Native ephemeral execution checks TMPDIR then /var/tmp; the selected filesystem
can therefore differ between machines. `--use-storage` is a useful disk override
even when an automatic choice would use RAM. Neither flag moves final installed
files, app documents/settings, or a daemon's store. Linux can swap tmpfs pages.

Native run/try checks metadata-based peak space plus a small workspace inode
allowance before download. It then counts actual archive paths, including
implicit parents and links, and checks remaining bytes/inodes/RAM before any
extraction. Byte size is not a file count. Checks do not reserve resources or
limit application memory. Failure discards the owned workspace; forced placement
never switches silently to another medium. Unhandled process death can leave a
workspace which a later invocation removes only after ownership and dead-PID checks.

Pipx's own freshness switch does not establish immediate environment removal
([Pipx run documentation](https://pipx.pypa.io/latest/tutorial/run-applications.html)).
Pacpin therefore puts a no-cache run's environment/shared libraries and pip/UV
download caches in its own private workspace. The 64 MiB starting check is not
a complete footprint guarantee for arbitrary Python compilation. Ordinary Pipx
runs retain Pipx's normal reuse policy.

This table reviews pacpin's invocation controls. Pacman compatibility flags
retain pacman's operation-specific rules; it does not promise a generic dry-run
for legacy forwarded operations or migrate every legacy external command caller.
