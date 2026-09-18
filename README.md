# pacpin

**Declarative Package Resolver & Upgrade Engine for Arch Linux / CachyOS**

`pacpin` is a high-performance native package resolver and upgrade engine written in Rust for Arch Linux and CachyOS. It combines declarative repository pinning, repo-level exclusions, stability delay buffers, companion split-package cascades, and transaction snapshot rollback journaling directly on top of `libalpm` via zero-copy C FFI.

---

## Key Features

1. **Declarative Repository Pins**: Lock packages or wildcard patterns (`linux-firmware*`, `amd-ucode`) to specific repositories (`core`, `extra`, `cachyos`, `aur`). Prevents blind upgrades from epoch-polluted or experimental third-party repos.
2. **True Resolver**: Synthesizes explicit qualified package targets (`repo/pkg`) instead of blind `pacman -Su` commands.
3. **Microsecond ALPM Engine**: Powered directly by `libalpm` C FFI. Database loading and evaluation across 16,000+ packages completes in under 30 milliseconds.
4. **Parallel AUR Querying**: Queries the AUR RPC v5 asynchronously over HTTP/TLS in background threads while ALPM sync databases are parsed.
5. **Stability Delay Buffers**: Enforce quarantine windows (e.g. `pacpin delay openssl 3`) with automatic reverse-dependency cascade holds to prevent ABI mismatches.
6. **Companion Cascade Detection**: Automatically detects split packages (sharing `%BASE%`) and prefix-related dependencies when pinning or installing to prevent version desynchronization.
7. **Transaction Snapshots & Rollback**: Logs transaction state to `~/.local/state/pacpin/history.jsonl` and enables 1-click package rollback (`pacpin rollback`) directly from the local pacman cache directory.
8. **Single-Confirmation**: Prompts once before execution; passes `--noconfirm` to low-level runners (`pacman` and `paru`).

---

## License

`pacpin` is licensed under the **GNU General Public License version 3 (GPLv3)**. See the [LICENSE](LICENSE) file for the full license text.

```
Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>
License GPLv3+: GNU GPL version 3 or later <https://gnu.org/licenses/gpl.html>
This is free software: you are free to change and redistribute it.
There is NO WARRANTY, to the extent permitted by law.
```

---

## Configuration (`~/.config/pacpin/config.toml`)

```toml
[features]
pinning = true
stability_delays = false
smart_orphans = true

[options]
helper = "paru"

[pins]
"amd-ucode" = "core"
"linux-firmware*" = "core"

[exclude]
cachyos = ["linux-firmware*"]

[delay]
# "openssl" = 3
```

---

## Commands Reference

### Package Management & Wrapper
| Command | Pacman / AUR Equivalent | Description |
| :--- | :--- | :--- |
| `pacpin check` (`pin check`, `pin -Qu`) | `pacman -Qu` | Check pending updates and verify active pin protections |
| `pacpin upgrade` (`pin -Syu`, `pin up`) | `pacman -Syu` | Safe system upgrade with True Resolver (`-y` refresh, `-n` dry-run, `-c` clean orphans) |
| `pacpin -S [repo/]pkg` (`pin install`) | `pacman -S`, `paru -S` | Install package(s); auto-pins `repo/pkg` with companion cascade (`-y` refresh, `-n` dry-run) |
| `pacpin remove <pkg...>` (`pin rm`, `pin -Rns`) | `pacman -Rns` | Remove package(s) and unneeded dependencies with **Smart Unpin** cleanup |
| `pacpin search <query...>` (`pin -Ss`) | `paru -Ss`, `pacman -Ss` | Search official repositories and the AUR simultaneously |
| `pacpin info <pkg...>` (`pin -Si`) | `paru -Si`, `pacman -Si` | View package metadata, dependencies, and upstream repository info |
| `pacpin clean` (`pin -Sc`) | `pacman -Sc`, `paru -Sc` | Clean cached package archives and AUR build directories |
| `pacpin orphans [-c]` (`pin autoremove`) | `pacman -Qdt` | Inspect or remove unneeded orphaned dependencies (`-c` to prompt selection) |
| `pacpin keep <pkg...>` (`pin adopt`) | `pacman -D --asexplicit` | Mark package(s) as explicitly installed to silence orphan warnings |

### Pinning & System Stability
| Command | Description |
| :--- | :--- |
| `pacpin pin <pkg> <repo>` | Lock package or pattern to a designated repository |
| `pacpin unpin <pkg>` | Remove a pin rule |
| `pacpin delay <pkg> <days>` | Set a stability delay buffer on a package |
| `pacpin undelay <pkg>` | Remove a stability delay buffer |
| `pacpin list` | List active pins, exclusions, delay rules, and matching packages |
| `pacpin history [id]` | View transaction history timeline or inspect a transaction |
| `pacpin rollback [id] [-n]` | Restore previous package versions from pacman cache |
| `pacpin init` (`pin setup`) | Interactive onboarding wizard to configure system preferences |
| `pacpin --version` | Display version and GPLv3 license information |

### Transparent Pacman Drop-in
`pacpin` serves as a complete drop-in wrapper. Any native pacman flag sequence (`-Q`, `-Qi`, `-Ql`, `-Qo`, `-F`, `-Fy`, `-U`, `-D`, etc.) passed to `pacpin` or `pin` is transparently handled with proper permission routing (non-root for queries, `sudo` for modifications).

---

## Building from Source

### Prerequisites
Building `pacpin` requires `pacman` / `libalpm` (C libraries and headers) and a Rust toolchain (1.75+):

```bash
# On Arch Linux / CachyOS
sudo pacman -S --needed base-devel git rust pacman
```

### Build & Install
```bash
cargo build --release
cp target/release/pacpin ~/.local/bin/pacpin
```
