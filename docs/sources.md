# Upstream release sources and builds (experimental)

GitHub, GitLab, HTTPS catalogs, and local catalogs share one release-selection,
verification and installation boundaries; optional install_dir selects retained directory installation. A configured alias is used
in `pin -S alias/package`. Direct provider targets use the same `provider:target`
shape as `flatpak:remote`: no hand-written alias is needed for reviewed recipes.
Signed Arch repositories still use pacman.conf/ALPM,
regardless of hosting; organizational distribution should prefer that route.

For actual compilation from verified source archives or release-tagged Git,
see [source builds and per-package optimization settings](source-builds.md).
Compilation is explicitly selected with `format = "source"`; defaults inherit
makepkg without extra optimization flags.

## GitHub: Codex alpha example

The built-in, reviewed Codex recipe can be installed and tracked directly:

```sh
pin -S github:openai/codex --channel alpha --dry-run
pin -S github:openai/codex --channel alpha
# equivalent hosted URL form:
pin -S https://github.com/openai/codex --channel alpha
```

`--channel` is only accepted for a direct source target; its default is
`stable`. The successful verified transaction records a deterministic internal
source alias and exact ownership, so normal `pin -Syu` checks it later. Dry
runs, declined confirmation, and failed transactions do not alter config.

Direct syntax is strict: `github:owner/project`, `gitlab:group/project` (nested
groups allowed), `https://github.com/owner/project`, and
`https://gitlab.com/group/project`. For self-hosted GitLab state intent
explicitly: `gitlab:https://git.company.example/group/project`; pacpin derives
that instance's `/api/v4` endpoint. Embedded credentials, fragments, query
strings, traversal, malformed projects, and bare company HTTPS project URLs
are rejected. Gitea, Bitbucket, generic Git URLs, remote scripts/PKGBUILDs, and
artifact URLs masquerading as catalogs are not supported.

Unknown GitHub/GitLab projects enter a TTY-only recipe interview. You explicitly
choose the package name, asset, archive/binary/native format, version/tag rules,
payload root, executable mappings, dependencies (including an explicit empty
answer), and checksum-file or fixed checksum. `--noconfirm` never authorizes
missing recipe input. The selected release still must provide a digest through
the API, checksum asset, or fixed-tag SHA-256 before installation.

HTTPS release manifests can be direct: `pin -S https://example/catalog.json`,
or `pin -S manifest:https://example/catalog`; local catalogs use
`pin -S manifest:/absolute/path/catalog.json`. A one-package catalog selects
that package; a multi-package catalog asks for an exact package name. Its recipe
is acquired with the same interview. Generic Git/Gitea/Bitbucket providers are
not implemented, but their HTTPS version-1 release manifests are supported.
Bare non-JSON company HTTPS paths are never assumed to be GitLab; use the
explicit `gitlab:https://host/group/project` form. That shorthand assumes a
root-mounted GitLab instance; for installations under a URL subpath or a custom
API base, use a configured `[sources]` GitLab source with its exact `url`.

Add to `~/.config/pacpin/sources.toml` (existing recipes in `config.toml` also work):

```toml
[sources.codex-alpha]
kind = "github"

[sources.codex-alpha.packages.openai-codex]
project = "openai/codex"
channel = "alpha"
version_prefix = "rust-v"
tag_pattern = "rust-v*-alpha.*"
asset = "codex-package-{arch}-unknown-linux-musl.tar.gz"
format = "archive"
depends = ["glibc", "alsa-lib", "bubblewrap", "ripgrep"]

[sources.codex-alpha.packages.openai-codex.binaries]
codex = "bin/codex"
```

```sh
pin -S codex-alpha/openai-codex --dry-run
pin -S codex-alpha/openai-codex
pin check
pin -Syu
```

This uses the full bundle, not just an executable. Recipes must match actual
archive layouts; optional `root = "app-directory"` selects a nested payload.
Successful installation saves an exact source pin. Source ownership remains
active even when native repository pinning is disabled. For native-packaged Sources, `pin unpin` relinquishes it. Directory Sources retain receipt inventory independently; unpinning is not removal or migration.

## GitLab/company instances

```toml
[sources.company]
kind = "gitlab"
url = "https://gitlab.example.org/api/v4"
token_env = "COMPANY_PACKAGE_TOKEN" # optional explicit environment variable

[sources.company.packages.myapp]
project = "team/subgroup/myapp"
asset = "myapp-{arch}.tar.gz"
checksum_asset = "SHA256SUMS"
format = "archive"
binaries = { myapp = "bin/myapp" }
```

Defaults: GitHub API `https://api.github.com`, GitLab API
`https://gitlab.com/api/v4`. Custom API bases support self-hosted instances.
GitLab links normally lack digests; use a checksum-file asset, a manifest, or
a fixed `tag` with explicit `sha256`. Checksum files use `sha256sum` format,
requiring exactly one entry for the selected artifact's exact filename.

Tokens use Bearer authentication and only the configured origin. Redirects
never forward tokens across origins or downgrade to HTTP. Private cross-origin
artifacts need signed URLs. Checksum-file downloads never send API tokens.

## Websites/local catalogs

```toml
[sources.website]
kind = "manifest"
url = "https://packages.example.org/catalog.json"

[sources.website.packages.myapp]
asset = "myapp-{arch}.tar.gz"
format = "archive"
binaries = { myapp = "bin/myapp" }
```

Version-1 JSON catalog:

```json
{
  "version": 1,
  "packages": {
    "myapp": [{
      "tag": "v1.2.3",
      "version": "1.2.3",
      "assets": [{
        "name": "myapp-x86_64.tar.gz",
        "url": "releases/myapp-x86_64.tar.gz",
        "sha256": "REPLACE_WITH_ACTUAL_64_HEX_DIGIT_SHA256"
      }]
    }]
  }
}
```

For local catalogs, use `kind = "local"` and an absolute catalog path as `url`.
Local artifact URLs must be relative paths inside that catalog's directory;
traversal and symlinks resolving outside it are rejected. HTTPS catalogs accept
relative or absolute HTTPS artifact URLs. Mounted company shares use the local
adapter; no company-specific updater is necessary.

## Common recipe options

- `format = "archive"`: tar with libarchive-supported compression. Without install_dir, payload is
  installed under `/opt/<package>` with declared `/usr/bin` links. No downloaded
  PKGBUILD or install script executes during packaging.
- `format = "binary"`: ELF executable/ELF AppImage; use
  `binaries = { myapp = "artifact" }`.
- `format = "native"`: Arch package; name, version, and architecture are checked
  before pacman installs it. A manifest may specify `package_version` for an
  Arch version differing from the semantic upstream version.
- `format = "source"`: compile upstream code with a local `build` table;
  see [the build recipe reference](source-builds.md).
- Channels: `stable` (default), `alpha`, `beta`, `prerelease`. Selection uses
  semantic version precedence, not tag string order. `version_prefix` defaults
  to `"v"`; manifests can supply `version` for unconventional tag spelling.
- Asset globs support `{arch}`, `{version}`, `{tag}`. Ambiguous matches fail.
- API selection considers the most recent 20 releases by default, choosing
  the greatest matching semantic version within that window. Set `release_pages`
  (1–20) in a package recipe to widen it. Exact `tag` uses a direct lookup, even
  for older releases. Manifests are checked in full within the response-size cap.
- `tag` fixes a release. Explicit `sha256` requires a fixed tag. Otherwise a
  digest from the API, catalog, or checksum-file asset is mandatory.
- Builds use unprivileged system `makepkg`; dependencies, conflicts, hooks,
  database locks, and native installation remain pacman's responsibility. Directory
  applications use receipt inventory and their own publication lock.

## Declarative rebuilding and update checks

```toml
[packages]
sources = ["codex-alpha/openai-codex", "company/myapp"]
```

`pin capture` records source-owned explicit packages separately from AUR.
`pin rebuild` restores missing packages without removing unrelated packages.
Carry both `[sources]` and `[packages]` to another machine. Source changes for
already-installed packages require `--enforce-repos`.

Source checks overlap native/AUR/external checks with at most four concurrent
source lookups. Execution uses the approved plan, not a newly fetched latest
release. Check failures hold installed packages and warn; they never fall back
to AUR/native repositories.

Source exclusions and `IgnorePkg` are respected; CLI `--ignore` also applies to
source upgrades. Nonzero stability-delay rules hold source packages with an
explicit warning: upstream release timestamp-based delay calculation is not yet
implemented. They are never silently bypassed.

## Trust and initial limitations

This is experimental. HTTPS/SHA-256 verify transport/integrity, **not independent
publisher authenticity**. Trust the publisher, catalog, and local recipe.
Native packages can contain root-executed hooks. Release sources are not sandboxes.

Limits: 10 MiB metadata per response, at most twenty API pages, 2 GiB downloads, existing
5 GiB/entry archive expansion limits. Unsafe archive paths/links/special entries
are rejected and special permission bits stripped. Private temporary directories
are cleaned on normal exit and SIGINT/SIGTERM, not guaranteed after SIGKILL/power loss.

Install sources separately from native/AUR targets. Explicit source installs
reject native transaction/refresh/sysupgrade flags; `pin -Syu` is the combined
update workflow. ZIP files, remotely supplied build commands, signature-policy
configuration, and private cross-origin token
downloads are not supported in this initial implementation.

## Retained applications on another filesystem

Set `install_dir` in a configured package recipe to install a relocatable
application without pacman. This supports `archive`, `binary` and compiled
`source` recipes; native package relocation is deliberately unsupported.
The entire payload is retained, not just the declared executable.

```toml
[sources.portable]
kind = "github"
[sources.portable.packages.myapp]
project = "owner/project"
asset = "myapp-{arch}.tar.xz"
format = "archive"
root = "myapp"                 # omit when the archive already has the right root
binaries = { myapp = "bin/myapp" }
install_dir = "/mnt/apps/myapp" # dedicated, initially absent application container
# checksum_asset = "SHA256SUMS" # needed if the API has no artifact digest
```

```sh
pin -S portable/myapp --dry-run
pin -S portable/myapp
pin source list
pin upgrade --sources-only
pin -R portable/myapp --dry-run
```

The executable is `/mnt/apps/myapp/current/bin/myapp`. Versions live beneath
`install_dir/versions`; `current` is a relative symlink switched atomically only
after verification. This works without directory-exchange support, including on
NTFS3. Symlink support, execute permissions, writable space, quotas and archive
layout still have to work on the chosen filesystem. Filesystem support does not
establish that every upstream application works on that filesystem.

The parent must already exist, be owned by the invoking user, and use its
canonical path. Installation directories must not overlap each other or protected
system directories. Runtime dependencies must already be installed; directory
installation never automatically installs dependencies or runs native hooks,
scriptlets, services or desktop/Steam registration. Use a relocatable upstream
build and register its `current` path with the consuming application yourself.
Do not create a second manager definition for the same installation. ALPM does
not register directory apps as dependency consumers. If a native runtime is needed
only by a directory app, keep it explicit (`pin keep PACKAGE`) so native orphan
cleanup does not consider it unused.

Receipt and container records bind source identity, destination and payload inode.
Update discovery reads these records rather than the pacman database. Successful
installs/adoptions retain a source pin; a missing mounted drive or missing receipt
for a pinned directory is an incomplete check, not “no updates.” `--keep-going`
can exclude the failed target while healthy Sources continue. Removal revalidates
ownership and deletes only the dedicated container. Native and directory targets
must be removed separately. `capture`, manifest `rebuild`, Sources scope, history
and version-change hooks include directory applications. Native cache rollback
cannot restore them. Installed trees are managed package files; keep personal
settings/documents outside the version directories.

### Reference an existing installation

Choose a **new** dedicated `install_dir`, then explicitly adopt the existing app:

```sh
pin source adopt portable/myapp --from /mnt/existing/myapp --version 1.2.3-1 --dry-run
pin source adopt portable/myapp --from /mnt/existing/myapp --version 1.2.3-1
```

Adoption checks declared executables and records the supplied version, but does
not claim upstream checksum verification. It creates a new container whose
`current` link references the existing app. The original directory receives no
files, is not moved, and is never deleted by updates, cleanup or removal of the
new container. After the next update, `current` selects a verified managed version;
the original remains yours to remove separately. Existing unmanaged destinations
are refused even with `--noconfirm`. Symlinked `--from` paths are resolved once.

### Vendor release versions, including Proton-CachyOS

Semantic versions remain the default. For portable releases using date/vendor
tags, opt in to `version_scheme = "arch"`, set `version_prefix`, and filter the
variant with `tag_pattern`. For example Proton-CachyOS's `cachyos-…-slr` tags can
use `version_prefix = "cachyos-"` and `tag_pattern = "cachyos-*-slr"`.
Hyphens in the remaining version become dots and `-1` is appended:
`11.0-20261020-slr` becomes `11.0.20261020.slr-1`. Selection uses ALPM version
ordering; normalization collisions are refused. This option requires
`install_dir` and a stable/prerelease channel. Asset patterns still receive the
unmodified stripped tag as `{version}`. Select an asset matching your CPU and
verify the archive root/executable mappings against that specific variant.

`--use-ram`/`--use-storage` select temporary download/build files, independently of
`install_dir`. Archives extract on the destination filesystem; old and new versions
coexist until publication succeeds. Old owned versions are then removed. Never
count `--no-cache` or `--cleanup=cache` as reclaiming an installed directory or
eliminating peak update space. In-flight generations are protected by the shared
cancellation registry; publication relinquishes cleanup ownership atomically.
