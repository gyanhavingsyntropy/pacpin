//! One backend decision feeds dispatch and option capabilities.
use super::TryOptions;
use crate::{config::Config, scratch::Policy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Backend<'a> {
    Native(Option<&'a str>),
    Aur,
    Source(&'a str),
    Export(&'a str),
    Flatpak,
    Nix,
    Pipx,
}

pub(super) fn resolve<'a>(
    prefix: Option<&'a str>,
    config: &Config,
    repositories: &[String],
) -> Result<Backend<'a>, String> {
    let Some(prefix) = prefix else {
        return Ok(Backend::Native(None));
    };
    let source = config.sources.contains_key(prefix);
    let export = config
        .manager_file
        .managers
        .get(prefix)
        .is_some_and(|definition| definition.adapter == "command");
    let native = repositories.iter().any(|repo| repo == prefix);
    let builtin = matches!(prefix, "aur" | "flatpak" | "nix" | "nixpkgs" | "pipx");
    if [source, export, native, builtin]
        .into_iter()
        .filter(|owner| *owner)
        .count()
        > 1
    {
        return Err(format!("Ambiguous try/run prefix '{prefix}': rename the configured source/export alias so it does not shadow another backend"));
    }
    Ok(if source {
        Backend::Source(prefix)
    } else if export {
        Backend::Export(prefix)
    } else {
        match prefix {
            "aur" => Backend::Aur,
            "flatpak" => Backend::Flatpak,
            "nix" | "nixpkgs" => Backend::Nix,
            "pipx" => Backend::Pipx,
            repo => Backend::Native(Some(repo)),
        }
    })
}

impl Backend<'_> {
    pub fn check_options(self, options: &TryOptions, scratch: Policy) -> Result<(), String> {
        if crate::cleanup::temporary() && matches!(self, Self::Flatpak | Self::Nix | Self::Pipx) {
            return Err("--cleanup=temporary is unsupported for this backend-owned runtime/store; its retained files cannot be classified as temporary build dependencies".into());
        }
        if options.no_cache && matches!(self, Self::Flatpak | Self::Nix) {
            return Err("--no-cache is unsupported for this run/try backend. Its installed runtime/store is not a disposable cache".into());
        }
        if scratch != Policy::Auto && matches!(self, Self::Flatpak | Self::Nix | Self::Pipx) {
            return Err("--use-ram/--use-storage is unsupported for this backend-owned store/environment: relocation cannot be guaranteed. Native/AUR/Sources/declared exports support workspace selection".into());
        }
        if matches!(self, Self::Nix | Self::Pipx) && !options.no_sandbox {
            return Err("Nix and Pipx run their programs on the host. Use 'pin run' or --no-sandbox to opt in to host execution".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_backend_declares_scratch_cache_and_isolation_support() {
        for backend in [
            Backend::Native(None),
            Backend::Aur,
            Backend::Source("s"),
            Backend::Export("e"),
            Backend::Flatpak,
            Backend::Nix,
            Backend::Pipx,
        ] {
            for scratch in [Policy::Auto, Policy::Ram, Policy::Storage] {
                for no_cache in [false, true] {
                    let options = TryOptions {
                        no_cache,
                        ..TryOptions::for_run()
                    };
                    let unsupported = no_cache
                        && matches!(backend, Backend::Flatpak | Backend::Nix)
                        || scratch != Policy::Auto
                            && matches!(backend, Backend::Flatpak | Backend::Nix | Backend::Pipx);
                    assert_eq!(
                        backend.check_options(&options, scratch).is_err(),
                        unsupported,
                        "{backend:?} {scratch:?} no_cache={no_cache}"
                    );
                }
            }
            assert_eq!(
                backend
                    .check_options(&TryOptions::default(), Policy::Auto)
                    .is_err(),
                matches!(backend, Backend::Nix | Backend::Pipx)
            );
        }
    }

    #[test]
    fn configured_prefixes_cannot_silently_choose_another_backend() {
        let mut config = Config::default();
        let source = crate::sources::Source {
            kind: crate::sources::Kind::Local,
            url: "/catalog.json".into(),
            token_env: None,
            packages: Default::default(),
        };
        config.sources.insert("source".into(), source.clone());
        assert_eq!(
            resolve(Some("source"), &config, &[]).unwrap(),
            Backend::Source("source")
        );
        config.sources.insert("flatpak".into(), source);
        assert!(resolve(Some("flatpak"), &config, &[]).is_err());
        assert!(resolve(Some("source"), &config, &["source".into()]).is_err());
        config.manager_file.managers.insert(
            "source".into(),
            crate::manager_config::Definition {
                adapter: "command".into(),
                ..Default::default()
            },
        );
        assert!(resolve(Some("source"), &config, &[]).is_err());
        assert_eq!(resolve(None, &config, &[]).unwrap(), Backend::Native(None));
    }
}
