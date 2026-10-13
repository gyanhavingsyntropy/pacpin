# Upstream release sources (experimental)

GitHub, GitLab, HTTPS catalogs, and local catalogs share one release-selection,
verification, packaging, and installation pipeline. A configured alias is used
in `pin -S alias/package`. Direct provider targets use the same `provider:target`
shape as `flatpak:remote`: no hand-written alias is needed for reviewed recipes.
Signed Arch repositories still use pacman.conf/ALPM,
regardless of hosting; organizational distribution should prefer that route.

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
active even when native repository pinning is disabled. `pin unpin` relinquishes it.

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

- `format = "archive"`: tar with libarchive-supported compression. Payload is
  installed under `/opt/<package>` with declared `/usr/bin` links. No downloaded
  PKGBUILD or install script executes during packaging.
- `format = "binary"`: ELF executable/ELF AppImage; use
  `binaries = { myapp = "artifact" }`.
- `format = "native"`: Arch package; name, version, and architecture are checked
  before pacman installs it. A manifest may specify `package_version` for an
  Arch version differing from the semantic upstream version.
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
  database locks, and installation remain pacman's responsibility.

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
update workflow. ZIP files, remote compilation/scripts, non-semantic version
ordering, signature-policy configuration, and private cross-origin token
downloads are not supported in this initial implementation.
