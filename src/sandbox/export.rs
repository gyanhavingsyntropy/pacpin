//! Explicit custom-manager exports share temporary runtime and cleanup policy.
use crate::preparation::{confirm_build, PackageRoot};
use super::TryOptions;

pub(super) fn try_export(
    config: &crate::config::Config,
    manager: &str,
    pkg: &str,
    args: &[String],
    options: &TryOptions,
) -> Result<i32, String> {
    let definition = config
        .manager_file
        .managers
        .get(manager)
        .ok_or("Unknown manager")?;
    if definition.enabled != Some(true) {
        return Err(format!("Manager '{manager}' is not enabled"));
    }
    let export = definition.ephemeral.as_ref().ok_or_else(|| format!("Manager '{manager}' has no temporary export contract; its upgrade command cannot safely substitute for one"))?;
    if options.dry_run {
        println!(":: Dry-run: {manager}/{pkg} would export a package tree in a {:?} workspace ({} MiB initial headroom), then {}. Command template: {:?}. No command executed.", crate::scratch::policy(), export.scratch_mib, if options.no_sandbox { "run with normal home/project writes" } else { "run in Bubblewrap isolation" }, export.command);
        return Ok(0);
    }
    confirm_build(
        &format!("Export {manager}/{pkg} using {:?}", export.command),
        options.noconfirm,
    )?;
    let view = crate::progress::session_view();
    let preparation = options.preparation();
    let root = PackageRoot::new(
        config,
        pkg,
        &preparation,
        export.scratch_mib.saturating_mul(1_048_576),
    )?;
    // Preparation sees host runtime read-only. The export destination is the
    // only persistent writable tree, and is not mounted back over /usr yet.
    let prepare_view = crate::execution::BuildWorkspace {
        root: root.work.join("empty"),
        overlay: root.view.overlay,
        readonly_paths: root.view.readonly_paths.clone(),
    };
    crate::integrations::export_temporary(definition, pkg, &root.root, &root.work, &prepare_view)?;
    crate::download::validate_symlink_traversal(&root.root, 32)?;
    let mut executables = Vec::new();
    for template in &export.executables {
        let path = root.root.join(template.replace("{id}", pkg));
        let actual = path
            .canonicalize()
            .map_err(|e| format!("Declared export executable is missing: {e}"))?;
        if !actual.starts_with(&root.root) || !crate::utils::is_executable_file(&actual) {
            return Err(
                "Export executable must be executable and confined to its private root".into(),
            );
        }
        executables.push(path);
    }
    let executable =
        super::entrypoints::select_executable(executables, pkg, options.bin.as_deref())?;
    drop(view);
    super::launch::launch(&root.root, &executable, pkg, args, options, false, true)
}
