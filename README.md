# pacpin

**Declarative Package Resolver & Upgrade Engine for Arch Linux / CachyOS**

`pacpin` is a high-performance native package resolver, system upgrade engine, and package sandboxing tool written in Rust for Arch Linux and CachyOS. It combines declarative repository pinning, repo-level exclusions, stability delay buffers, companion split-package cascades, vendor stickiness, multi-package manager unification, post-upgrade restart inspection, ephemeral sandboxing, and transaction snapshot rollback journaling directly on top of `libalpm` via zero-copy C FFI.

---

## Key Features

1. **Declarative Repository Pins**: Lock packages or wildcard patterns (`linux-firmware*`, `amd-ucode`) to specific repositories (`core`, `extra`, `cachyos`, `aur`). Prevents blind upgrades from epoch-polluted or experimental third-party repos.
2. **True Resolver Engine**: Synthesizes explicit qualified package targets (`repo/pkg`) instead of blind `pacman -Su` commands, preventing unexpected repository hopping.
3. **Opt-In Vendor Stickiness**: Keep packages bound to their originating repository (`%INSTALLED_DB%`) during upgrades so they don't unexpectedly jump between sync repositories (e.g. `core` ➔ `cachyos`), displaying a distinct `[sticky]` badge in transactions.
4. **BIOS-Style Repository Priority Menu**: Interactively reorder your system's repository search priority using arrow keys and instant promotion/demotion (`pacpin repos`).
5. **Ephemeral Package Sandbox (`pacpin try [repo/]pkg [args...]`)**: Like `nix run`, download and execute tools in a kernel-isolated container using Bubblewrap (`bwrap`) with unshared namespaces, read-only system mounts, private in-memory `/tmp`, fail-closed SHA256 integrity verification, archive traversal guards, and zero host residue upon exit (supports native Arch repos, `flatpak/<app-id>`, `nix/<pkg>`, and `pipx/<pkg>`).
6. **Post-Upgrade Restart Inspector (`needrestart`)**: Automatically inspect running processes holding deleted `.so` libraries in RAM (`/proc/*/maps`) and verify running kernel vs installed modules (supporting Arch, CachyOS, XanMod, RT, TKG, and Bochs kernels), advising on exact service restart commands (`sudo systemctl restart <srv>`) with zero noise when nothing needs restarting.
7. **Scrollable Checkbox TUI with Batch Shortcuts**: Interactive viewport checklist (`Space` toggle, `Enter` confirm) with one-key batch actions (`[a]` All, `[n]` None, `[p]` Pure Only, `[i]` Invert, `/` Search) for orphan management and setup.
8. **Unified Multi-Package Manager Integrations**: Simultaneously check, refresh, and execute updates for Flatpak, Nix, and Pipx alongside Pacman and AUR in a single transaction view.
9. **Batch Pinning from Files**: Pin multiple packages at once or import a package list from a text file (`pacpin pin <repo> -f <file>`).
10. **Stability Delay Buffers**: Enforce quarantine windows (e.g. `pacpin delay linux 3`) with automatic reverse-dependency cascade holds to prevent ABI mismatches.
11. **Companion Cascade Detection**: Automatically detects split packages (sharing `%BASE%`) and prefix-related dependencies when pinning or installing to prevent version desynchronization.
12. **Transaction Snapshots & Rollback**: Logs transaction state to `~/.local/state/pacpin/history.jsonl` and enables 1-click package rollback (`pacpin rollback`) directly from the local pacman cache.
13. **Single-Confirmation**: Prompts once before execution; passes `--noconfirm` to low-level runners (`pacman` and `paru`).

---

## Configuration (`~/.config/pacpin/config.toml`)

```toml
[features]
pinning = true
vendor_stickiness = false
stability_delays = false
smart_orphans = true
integrations = false
shell_aliases = true

[options]
helper = "paru"

repo_order = ["core", "cachyos", "extra", "multilib"]

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
```

---

## Commands Reference

### Core Package Operations
| Command | Pacman / AUR Equivalent | Description |
| :--- | :--- | :--- |
| `pacpin check` (`pin check`, `pin -Qu`) | `pacman -Qu` | Check pending updates and verify active pin protections |
| `pacpin upgrade` (`pin -Syu`, `pin up`) | `pacman -Syu` | Safe system upgrade with True Resolver (`-y` refresh, `-n` dry-run, `-c` clean orphans) |
| `pacpin -S [repo/]pkg` (`pin install`) | `pacman -S`, `paru -S` | Install package(s); auto-pins `repo/pkg` with companion cascade (`-y` refresh, `-n` dry-run) |
| `pacpin -Sp <pkg...>` (`pin -Sp`) | `pacman -Sp` | Natively resolve and print package and missing dependency download URIs via ALPM |
| `pacpin -Sl [repo...]` (`pin -Sl`) | `pacman -Sl` | List packages in sync repositories with installation status |
| `pacpin -Sg [group...]` (`pin -Sg`) | `pacman -Sg` | List package groups or packages in a designated group |
| `pacpin remove <pkg...>` (`pin rm`, `pin -Rns`) | `pacman -Rns` | Remove package(s) and unneeded dependencies with **Smart Unpin** cleanup |
| `pacpin search <query...>` (`pin -Ss`) | `paru -Ss`, `pacman -Ss` | Search official repositories and the AUR simultaneously |
| `pacpin info <pkg...>` (`pin -Si`) | `paru -Si`, `pacman -Si` | View package metadata, dependencies, and upstream repository info |
| `pacpin clean` (`pin -Sc`) | `pacman -Sc`, `paru -Sc` | Clean cached package archives and AUR build directories |

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

### System Maintenance & Hygiene
| Command | Description |
| :--- | :--- |
| `pacpin orphans [-c]` (`pin autoremove`) | Inspect or remove unneeded orphaned dependencies with scrollable interactive checkbox TUI |
| `pacpin keep <pkg...>` (`pin adopt`) | Mark package(s) as explicitly installed to silence orphan warnings |
| `pacpin needrestart` (`pin restart-check`) | Inspect processes holding outdated libraries (`/proc/*/maps`) or replaced kernel in RAM |

### Power Tools & Sandboxing
| Command | Description |
| :--- | :--- |
| `pacpin try [--no-sandbox] [repo/]pkg [args...]` (`pin run`) | Run package in an ephemeral Bubblewrap container (`--no-sandbox` / `--bare` runs with host environment isolation) |
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
Building `pacpin` requires `pacman` / `libalpm` (C libraries and headers), `bubblewrap` (for container sandboxing), and a modern Rust toolchain (Rust 1.85+, Edition 2024 required by `alpm 5.0.2`):

```bash
# On Arch Linux / CachyOS
sudo pacman -S --needed base-devel git rust pacman bubblewrap
```

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
