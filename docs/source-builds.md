# Source compilation and optimization profiles

Source builds are an explicit `format = "source"` recipe in
`~/.config/pacpin/sources.toml`. Existing prebuilt recipes keep their current
behavior. Pacpin does not guess build systems or replace a prebuilt Codex recipe
with compilation. The direct-target interview remains for prebuilt recipes;
configure builds in TOML and install their `alias/package` target.

## GitHub example

This builds cmark from the exact commit of the selected semantic GitHub release:

```toml
[sources.cmark-build]
kind = "github"

[sources.cmark-build.packages.cmark-local]
project = "jgm/cmark"
format = "source"
depends = ["glibc"]
binaries = { cmark-local = "bin/cmark" }

[sources.cmark-build.packages.cmark-local.build]
git = "https://github.com/jgm/cmark.git"
output = "stage"
scratch_mib = 256
scratch_inodes = 8192
makedepends = ["cmake", "gcc", "make"]
jobs = 2
commands = [
  ["cmake", "-S", "{source}", "-B", "build", "-DCMAKE_INSTALL_PREFIX={output}", "-DCMARK_TESTS=OFF", "-DCMARK_SHARED=OFF"],
  ["cmake", "--build", "build"],
  ["cmake", "--install", "build"]
]

[sources.cmark-build.packages.cmark-local.build.optimization]
cpu = "system" # default: inherit makepkg; add no optimization flags
# cpu = "native" # optional: target the build CPU; may not run on another CPU
# level = 2      # optional: C/C++ -O2 and Rust opt-level=2; supported levels 0..3
# lto = false    # optional: override makepkg's LTO policy for this package
```

```sh
pin -S cmark-build/cmark-local --dry-run
pin -S cmark-build/cmark-local --use-storage
pin upgrade --sources-only
```

Install declared build dependencies explicitly first. Missing plain-name
`makedepends` or tools fail preflight before a full upgrade starts native
transactions. Pacpin does not silently install a build toolchain. Runtime
dependencies in `depends` remain checked by pacman when installing the package.

Commands stage an installable tree under `output`; it is installed beneath
`/opt/<package>` with the declared `/usr/bin` links. `root` optionally selects a
subdirectory of the checkout/archive; it must stay inside that tree. `jobs`
sets make/Ninja/CMake/Cargo parallelism preferences; commands can override them.

## Exact Git inputs

The selected release tag resolves to a full commit **before approval**. Fetch and
build use that commit even if the tag subsequently moves. Annotated tags are
peeled. Optional `submodules = true` fetches recorded submodule revisions with
HTTPS only. An optional full 40-digit `revision` requires a fixed recipe `tag` and
deliberately fixes that source/version pairing. This implementation tracks
semantic releases, not arbitrary branch heads or AUR-style VCS pkgver.

Git repositories must use public HTTPS; Git credential stores and Git hooks are
disabled, and API `token_env` is not forwarded. Local catalogs can explicitly
name an absolute Git directory for development. Git preparation runs in the
foreground Sources phase. Source-archive downloads can use the existing prefetch
overlap; compilation never runs in a prefetch worker.

## Verified source archives and other build systems

Published source-code archives use the existing asset and digest selection:

```toml
[sources.my-project]
kind = "github"
[sources.my-project.packages.myapp]
project = "example/myapp"
format = "source"
asset = "myapp-{version}-source.tar.gz"
checksum_asset = "SHA256SUMS"
root = "myapp-1.2.3" # optional; match the actual selected archive's layout
binaries = { myapp = "bin/myapp" }
[sources.my-project.packages.myapp.build]
output = "stage"
scratch_mib = 1024
commands = [
  ["cargo", "build", "--release", "--locked", "--target-dir", "build"],
  ["mkdir", "-p", "{output}/bin"],
  ["cp", "build/release/myapp", "{output}/bin/myapp"]
]
```

Archives need a SHA-256 from the API, catalog, checksum asset or fixed-tag recipe.
GitHub's automatically generated source archives do not bypass verification when
no digest is supplied; use an approved Git checkout instead.

Each nested array is literal argv. Substitutions are limited to `{source}`,
`{output}` (absolute private paths) and `{version}` (Arch package version).
Shell syntax is not implicitly evaluated; `sh -c` is an explicit shell command
chosen by the author. Build commands and upstream scripts run with the regular
user's permissions. This is not a security sandbox: review the source and recipe
as with AUR builds. Build instructions come only from local TOML, not a remote
catalog or downloaded PKGBUILD.

## Conservative optimization defaults

- Default `cpu = "system"` inherits makepkg. No CPU, level, LTO, PGO, BOLT or
  fast-math override is added by pacpin. Existing user settings still apply.
- Opt-in `cpu = "native"` requests `-march=native` on x86, `-mcpu=native` on ARM,
  and Rust `-C target-cpu=native`. Level and LTO are separate optional overrides.
  Unsupported settings fail validation; there is no silent fallback.
- The original makepkg config, conf.d and user overrides load through a temporary
  profile. Global files are never edited. Compiler settings are printed during
  compilation. Provenance records the requested profile, input identity and
  recipe digest, without copying literal build commands into the package.
- Build systems may override or ignore flags; a requested profile is not proof of
  effective flags or a performance improvement. CPU targeting needs no second
  compilation pass. LTO can increase time and RAM; PGO/training remains explicit
  package-specific commands. No universal build-time percentage is promised.

## Capacity, cleanup and transaction boundaries

`scratch_mib` is mandatory additional build headroom; `scratch_inodes` defaults
to 4096. These are estimates, not an enforced quota or peak-usage guarantee.
Checks include filesystem space, quotas, inodes and the RAM reserve before work
and at build/package boundaries. Archive expansion and actual compiled output
are inspected separately. Other processes or underestimated builds can still
exhaust resources.

`--use-ram` / `--use-storage` apply to download, checkout, compilation and
packaging. Forced media never silently fall back. RAM retains the 2.5 GiB OS
reserve; final installed files and native dependencies still require root space.

Sources removes successful and failed workspaces, including handled interruption.
Shared owned-resource cleanup handles makepkg's restricted failed-build directory
permissions without chmodding symlink targets. Common Cargo/Pip/npm/XDG temporary
caches stay within the workspace. Source-only `--no-cache` leaves unrelated native
and AUR caches alone. Arbitrary scripts, external toolchain installers and daemons
may have effects elsewhere; pacpin does not promise to undo those effects.

Compilation happens once through makepkg. The staged output and declared
executables are inspected; packaging then uses the shared packaging-only recipe.
Only a successful, identity-checked package reaches elevated `pacman -U`.
Compilation failure, missing output, escaped executables and invalid packages
cannot install the source package. Generated packages have no upstream install
scriptlet. Native installation failures can still partially apply files and are
fatal.

Source-code builds are excluded from independent continuation after AUR failure:
arbitrary build commands have unverified dependency effects. Prebuilt Sources
retain their existing guarded continuation contract. Upgrade scope, hooks,
approval, footer and ownership use the ordinary Sources pipeline.
