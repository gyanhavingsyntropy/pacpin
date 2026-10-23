//! Adapt package-producing backends to the common temporary launcher.
use super::{
    package_root::{confirm_build, PackageRoot},
    TryOptions,
};

pub(super) fn try_aur(
    config: &crate::config::Config,
    pkg: &str,
    args: &[String],
    options: &TryOptions,
) -> Result<i32, String> {
    if options.dry_run {
        let item = crate::aur::query_aur_checked(&[pkg.into()])?
            .remove(pkg)
            .ok_or_else(|| format!("AUR package '{pkg}' not found"))?;
        println!(":: Dry-run: aur/{pkg} ({}) would fetch its PKGBUILD, review it, resolve private build/runtime dependencies and build without installation. Build size is unknown; selected scratch policy: {:?}. No recipe executed or workspace created.", item.version, crate::scratch::policy());
        return Ok(0);
    }
    let view = crate::progress::session_view();
    let mut root = PackageRoot::new(config, pkg, options, 128 * 1024 * 1024)?;
    let archive = root.build_aur(pkg)?;
    let executable = root.executable(&archive, pkg)?;
    drop(view);
    super::launch::launch(&root.root, &executable, pkg, args, options, false, true)
}

pub(super) fn try_source(
    config: &crate::config::Config,
    alias: &str,
    pkg: &str,
    args: &[String],
    options: &TryOptions,
) -> Result<i32, String> {
    crate::sources::validate_config(config)?;
    let plan = crate::sources::plan(config, &format!("{alias}/{pkg}"))?;
    let mut dependencies = plan.recipe.depends.clone();
    let mut bytes = 128 * 1024 * 1024;
    if let Some(build) = &plan.recipe.build {
        dependencies.extend(build.makedepends.clone());
        bytes = build.scratch_mib.saturating_mul(1_048_576);
    }
    println!(
        ":: Temporary source: {alias}/{pkg} -> {} ({}); no installation or pin changes.",
        plan.version, plan.tag
    );
    if options.dry_run {
        println!(":: Dry-run: {:?} temporary build/runtime files; private dependencies: {}. No download, build or app execution.", crate::scratch::policy(), dependencies.join(", "));
        return Ok(0);
    }
    confirm_build(
        &format!("Prepare source {alias}/{pkg} from the configured local recipe"),
        options.noconfirm,
    )?;
    let view = crate::progress::session_view();
    let mut root = PackageRoot::new(config, pkg, options, bytes)?;
    root.ensure_dependencies(&dependencies)?;
    let prepared = crate::progress::temporary_source_preparation(|| {
        crate::sources::prepare_temporary(config, &plan, &root.view)
    })?;
    root.add_archive(&prepared.archive, pkg)?;
    let executable = root.executable(&prepared.archive, pkg)?;
    drop(view);
    super::launch::launch(&root.root, &executable, pkg, args, options, false, true)
}
