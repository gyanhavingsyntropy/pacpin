# pacpin

**Declarative Package Resolver & Upgrade Engine for Arch Linux / CachyOS**

> [!WARNING]
> **Experimental pre-release (`3.2.0-alpha.1`).** Known bugs remain, and the
> published `v3.1.0` build should not be considered stable. Pacpin has not
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
8. **Unified Multi-Package Manager Integrations**: Simultaneously check, refresh, and execute updates for Flatpak, Nix, and Pipx alongside Pacman and AUR in a single transaction view.
9. **Batch Pinning from Files**: Pin multiple packages at once or import a package list from a text file (`pacpin pin <repo> -f <file>`).
10. **Stability Delay Buffers**: Enforce quarantine windows (e.g. `pacpin delay linux 3`) with automatic reverse-dependency cascade holds to prevent ABI mismatches.
11. **Companion Cascade Detection**: Automatically detects split packages (sharing `%BASE%`) and prefix-related dependencies when pinning or installing to prevent version desynchronization.
12. **Transaction Snapshots & Rollback**: Logs transaction state to `~/.local/state/pacpin/history.jsonl` and enables 1-click package rollback (`pacpin rollback`) directly from the local pacman cache.
13. **Single-Confirmation**: Prompts once before execution; passes `--noconfirm` to low-level runners (`pacman` and `paru`).
14. **Upstream Release Sources (experimental)**: GitHub, GitLab, HTTPS catalogs, and local/company catalogs share declarative recipes and the same verified native-install pipeline. Use `pin -S alias/package`; [configuration and trust limits](docs/sources.md).

---

## Configuration (`~/.config/pacpin/config.toml`)

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

The final plan summary follows dependency and removal details. Its native counts and sizes include the prepared transaction's additional packages: for example, `41 upgrades + 7 new dependencies`, with no `+ 0` when there are none. Sources and other managers retain unknown sizes rather than appearing to add zero bytes.

`--no-cache` cleans only the native/AUR caches involved in successful manager transactions. Sources remove their temporary workspaces separately. Cleanup reports completion, failure, or a reason for skipping; failed-manager archives remain available for recovery. A requested cleanup failure makes the command fail even when package installation succeeded. Cleanup finishes before restart advice.

Cache removal after installation does not eliminate temporary installation space. pacpin checks estimated native root/cache capacity before downloading and again before installing; source builds also check their final installation destination. Downloads, build scratch, final net growth, and temporary rewrite space are separate requirements. These estimates cannot guarantee space for every snapshot, quota, concurrent writer, or package hook.

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
| `pacpin init [--reset]` | Interactive onboarding wizard to configure preferences from scratch |
| `pacpin capture [-n|--force]` | Save explicitly installed packages and Flatpak apps to `[packages]` (`-n` previews) |
| `pacpin rebuild [-n|--noconfirm] [--enforce-repos]` | Install missing listed packages without deleting extras; optionally reinstall from declared repositories |

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
> - **Credential & Home Isolation**: Sandboxed containers run in unshared Linux user, mount, IPC, PID, and network namespaces with read-only root filesystems and a private in-memory tmpfs over `$HOME`. When running `pacpin try` from `$HOME` itself, the working directory is deliberately not overlaid to keep host credentials (`~/.ssh`, `~/.gnupg`, tokens) strictly isolated.
> - **Hardware & GUI Passthrough**: Flags like `--gui` and `--audio` pass through host display sockets (`/tmp/.X11-unix`, Wayland) and audio nodes (`/dev/snd`), which intentionally broadens the trust boundary for desktop interaction.
> - **Archive & Input Verification**: Ephemeral packages are downloaded with fail-closed SHA256 integrity verification, streamed through structured `tar` header validation, and guarded against symlink/hardlink directory escapes.

### Transparent Pacman Drop-in
`pacpin` serves as a complete drop-in wrapper. Any native pacman flag sequence (`-Syu`, `-S -y -u`, `-Ss`, `-Si`, `-Sc`, `-Sp`, `-Sl`, `-Sg`, `-Sw`, `-T`, `-Q`, `-Qi`, `-Ql`, `-Qo`, `-F`, `-Fy`, `-U`, `-D`, etc.) passed to `pacpin` or `pin` is transparently handled with proper permission routing (non-root for queries, `sudo` for modifications).

---

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
