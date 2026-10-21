// pacpin - Declarative Package Resolver & Upgrade Engine for Arch Linux / CachyOS
// Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::process::{exit, Command, Stdio};
use std::thread;

use colored::Colorize;

use crate::commands::check_pacman_lock;
use crate::config::Config;
use crate::db::{AlpmManager, OrphanPackage};
use crate::integrations::IntegrationsManager;
use crate::journal::TransactionJournal;
use crate::orphans::OrphanManager;
use crate::pm;
use crate::preview::{
    RequestedStatus, prepare_native_preview, strip_repo,
};
use crate::resolver::{ResolveResult, ResolvedPackage, ResolverEngine};
use crate::restart;
use crate::ui::{self, print_banner};

mod planning;
mod preflight;
mod execution_plan;
#[cfg(test)]
mod tests;

use planning::{
    collect_ignored_names, ignored_names, requested_aur_resolved_package,
    requested_resolved_package, resolve_requested_targets, validate_upgrade_targets,
};

pub fn cmd_upgrade(
    config: &Config,
    dry_run: bool,
    refresh: bool,
    noconfirm: bool,
    autoremove: bool,
    extra_flags: &[String],
    requested: &[String],
) {
    check_pacman_lock();
    print_banner();

    let scope = match crate::runtime::UpgradeScope::parse(extra_flags) {
        Ok(scope) => scope, Err(error) => { eprintln!("Error: {error}"); exit(2); }
    };
    let keep_going = crate::runtime::has_control(extra_flags, "--keep-going");
    if !scope.includes_native() && autoremove {
        eprintln!("Error: scoped upgrades cannot include orphan removal; use pin orphans -c separately"); exit(2);
    }
    let mut provider_config = match crate::manager_config::select_flags(config, extra_flags) {
        Ok(config) => config, Err(error) => { eprintln!("Error: {error}"); exit(2); }
    };
    for argument in crate::runtime::custom_flags(extra_flags) {
        let key = argument.trim_start_matches("--").split('.').next().unwrap_or("");
        if !scope.includes_runner(match key { "flatpak" => "Flatpak", "nix" => "Nix", "pipx" => "Pipx", _ => key }) {
            eprintln!("Error: {argument} is outside the selected upgrade scope"); exit(2);
        }
    }
    if matches!(scope, crate::runtime::UpgradeScope::Sources | crate::runtime::UpgradeScope::Aur) {
        provider_config.features.integrations = false;
        for definition in provider_config.manager_file.managers.values_mut() { definition.enabled = Some(false); }
    }
    if scope == crate::runtime::UpgradeScope::Aur { provider_config.sources.clear(); }
    let ext_providers = crate::progress::operation(crate::progress::Manager::Checks, "Detecting package managers", || IntegrationsManager::get_active_providers(&provider_config))
        .into_iter().filter(|p| scope.includes_runner(p.name())).collect::<Vec<_>>();
    let mut discovery_failures = Vec::new();
    if !scope.includes_native() && refresh {
        println!(":: Scoped upgrade: native repository databases are not refreshed. AUR builds may still require native build dependencies.");
    }

    // Managers discover independently: only their own refresh is a prerequisite.
    // Sources can check while native databases and Flatpak metadata refresh.
    let ext_providers_for_check = ext_providers.clone();
    let ext_handle = thread::spawn(move || {
        IntegrationsManager::discover_parallel(&ext_providers_for_check, refresh && !dry_run)
    });

    if refresh && !dry_run && scope.includes_native() {
        println!(
            ":: Refreshing package databases ({} pacman -Sy)...",
            config.options.privilege_tool
        );

        let status = pm::refresh_databases(config);
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                // Finish bounded workers before exit; never strand a configured
                // metadata command after native discovery fails.
                let _ = crate::progress::operation(crate::progress::Manager::Checks, "Checking external updates", || ext_handle.join());
                eprintln!(
                    "{}",
                    format!(
                        "Error: Database refresh failed with exit code {}.",
                        s.code().unwrap_or(1)
                    )
                    .red()
                );
                exit(s.code().unwrap_or(1));
            }
            Err(e) => {
                let _ = crate::progress::operation(crate::progress::Manager::Checks, "Checking external updates", || ext_handle.join());
                eprintln!("{}", format!("Failed to run {} pacman: {}", config.options.privilege_tool, e).red());
                exit(1);
            }
        }
    }

    let mut manager = match crate::progress::operation(crate::progress::Manager::Checks, "Reading package databases", || AlpmManager::with_repo_order(&config.repo_order)) {
        Ok(m) => m,
        Err(e) => {
            let _ = crate::progress::operation(crate::progress::Manager::Checks, "Checking external updates", || ext_handle.join());
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };
    for warn in if matches!(scope, crate::runtime::UpgradeScope::Sources | crate::runtime::UpgradeScope::External) { Vec::new() } else { manager.verify_syncdbs() } {
        eprintln!("{}", format!(":: Warning: {}", warn).yellow());
    }
    let resolver = ResolverEngine::new(&manager);
    let mut res = if matches!(scope, crate::runtime::UpgradeScope::Sources | crate::runtime::UpgradeScope::External) {
        ResolveResult { check_errors: Vec::new(), installed_count: manager.handle().localdb().pkgs().len(), repos: Vec::new(), packages: std::collections::HashMap::new(), updates: Vec::new(), held_packages: Vec::new(), unresolved_pins: Vec::new() }
    } else { crate::progress::operation(crate::progress::Manager::Checks, "Checking package updates", || resolver.resolve_all(config)) };
    discovery_failures.extend(res.check_errors.iter().cloned());
    res.updates.retain(|p| if p.candidate.as_ref().is_some_and(|c| c.is_aur) { scope.includes_aur() } else { scope.includes_native() });
    if !scope.includes_native() {
        res.held_packages.retain(|p| scope.includes_aur() && p.candidate.as_ref().is_some_and(|c| c.is_aur));
        res.unresolved_pins.retain(|(_, repo)| scope.includes_aur() && repo == "aur");
    }
    // Ignored packages leave the plan entirely (display, preview, targets,
    // journal): pacman answers "Install anyway?" affirmatively for
    // explicitly listed targets under --noconfirm, so forwarding the flags
    // alone would silently void ignores. The flags are still forwarded as a
    // backstop for dependency resolution.
    let ignored = collect_ignored_names(&manager, extra_flags);
    if !ignored.is_empty() {
        let mut names: Vec<&String> = ignored.iter().collect();
        names.sort();
        let shown: Vec<&String> = names.iter().take(12).copied().collect();
        let mut msg = format!(
            ":: Ignoring {} package(s): {}",
            names.len(),
            shown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        if names.len() > shown.len() {
            msg.push_str(&format!(" (and {} more)", names.len() - shown.len()));
        }
        println!("{}", msg.yellow());
        res.updates.retain(|update| !ignored.contains(&update.name));
        res.held_packages
            .retain(|held| !ignored.contains(&held.name));
    }
    let checked = crate::progress::operation(crate::progress::Manager::Checks, "Checking external updates", || ext_handle.join());
    let (mut external_updates, check_failures) = match checked {
        Ok(report) => report.into_parts(),
        Err(_) => { eprintln!("Error: External update worker failed"); exit(1); },
    };
    discovery_failures.extend(check_failures);
    external_updates.retain(|update| update.runner != "Sources" || !crate::sources::ignored(&update.name, extra_flags));
    for (key, definition) in &provider_config.manager_file.managers {
        let name = match key.as_str() { "flatpak" => "Flatpak", "nix" => "Nix", "pipx" => "Pipx", _ => key };
        if !definition.selected_flags.is_empty() && !external_updates.iter().any(|u| u.runner == name) {
            println!(":: {name}: custom flags skipped because this manager has no approved updates.");
        }
    }

    // Explicitly requested targets (`pin -Syu <pkg...>`) join the upgrade in
    // their own box. Source-owned and direct upstream targets are rejected
    // here with guidance: they are already tracked by `pin -Syu`, and forcing
    // one uses `pin -S`.
    if !requested.is_empty()
        && extra_flags
            .iter()
            .any(|flag| flag == "--channel" || flag.starts_with("--channel="))
    {
        eprintln!(
            "{}",
            "Error: --channel is only valid with a direct github:/gitlab: source target".red()
        );
        exit(1);
    }
    let selected_requests = preflight::eligible_requests(config, &manager, requested, &discovery_failures);
    let mut requested_targets = match resolve_requested_targets(config, &manager, &res, &selected_requests, &ignored) {
        Ok(targets) => targets,
        Err(error) => {
            eprintln!("{}", format!("Error: {error}").red());
            exit(1);
        }
    };
    if !scope.includes_native() && !requested_targets.native.is_empty()
        || !scope.includes_aur() && !requested_targets.aur.is_empty() {
        eprintln!("Error: requested targets do not belong to the selected upgrade scope"); exit(2);
    }
    let no_cache = config.options.no_cache
        || extra_flags.iter().any(|flag| flag == "--no-cache" || flag == "--clean-cache");
    let manager_flags = crate::runtime::manager_flags(extra_flags);
    let actionable = !res.updates.is_empty() || !external_updates.is_empty()
        || requested_targets.items.iter().any(|item| item.status != RequestedStatus::AlreadyCurrent);
    let aur_preflight = if actionable { preflight::aur_targets(&res, &requested_targets) } else { Vec::new() };
    let eligibility = match preflight::prepare(config, &ext_providers, &external_updates, aur_preflight, &manager_flags,
        preflight::Options { policy: crate::scratch::policy(), no_cache, keep_going }, discovery_failures) {
        Ok(plan) => plan,
        Err(error) => { eprintln!("Error: {error}"); exit(2); }
    };
    eligibility.apply(&mut res, &mut requested_targets, &mut external_updates);
    let orphans = if !eligibility.incomplete() && scope.includes_native() && (config.features.smart_orphans || autoremove) {
        crate::progress::operation(crate::progress::Manager::Checks, "Checking orphan candidates", || manager.get_orphans(&res.updates))
    } else {
        Vec::new()
    };

    let requested_names: HashSet<String> = requested_targets
        .items
        .iter()
        .map(|item| item.name.clone())
        .collect();
    let res_view = ResolveResult {
        check_errors: res.check_errors.clone(),
        installed_count: res.installed_count,
        repos: res.repos.clone(),
        packages: std::collections::HashMap::new(),
        updates: res
            .updates
            .iter()
            .filter(|update| !requested_names.contains(&update.name))
            .cloned()
            .collect(),
        held_packages: res.held_packages.clone(),
        unresolved_pins: res.unresolved_pins.clone(),
    };
    if !requested_targets.items.is_empty() {
        ui::render_requested_box(&requested_targets.items);
    }
    let updates = res_view.updates.clone();
    let orphan_list_changed = if !eligibility.incomplete() && scope.includes_native() && config.features.smart_orphans {
        OrphanManager::needs_review(&orphans)
    } else {
        false
    };

    let actionable_requested = requested_targets
        .items
        .iter()
        .any(|item| item.status != RequestedStatus::AlreadyCurrent);

    if updates.is_empty()
        && external_updates.is_empty()
        && (requested_targets.items.is_empty() || !actionable_requested)
    {
        if eligibility.incomplete() {
            eligibility.preview();
            println!(":: No eligible package updates remain; no transaction will be started.");
            eligibility.report_incomplete();
            exit(2);
        }
        ui::render_transaction_view(&res_view, &config.options.helper, &external_updates);
        if !requested_targets.items.is_empty() {
            println!(
                "\n{}",
                ":: All requested packages are already current; nothing to do.".cyan()
            );
        }
        if autoremove || orphan_list_changed {
            println!("\n{}", ":: No package updates; orphan cleanup is a separate action.".cyan());
            ui::render_orphans_summary(&orphans);
        }
        if (autoremove || orphan_list_changed) && !dry_run && !orphans.is_empty() {
            let selected_orphans = if !noconfirm {
                ui::prompt_orphan_selection(&orphans, false)
            } else {
                noconfirm_orphan_selection(&orphans, autoremove)
            };

            if !noconfirm || !selected_orphans.is_empty() {
                if let Err(error) = OrphanManager::record_selection(&selected_orphans) {
                    eprintln!(":: {error}; cleanup aborted before removal.");
                    exit(1);
                }
            }
            let result = OrphanManager::execute_removal(&selected_orphans, &config.options.privilege_tool);
            let removal_ok = result.is_ok();
            let removed = result.unwrap_or_else(|error| { eprintln!(":: Warning: {error}"); Vec::new() });
            let remaining: Vec<OrphanPackage> = orphans
                .iter()
                .filter(|o| !removed.contains(&o.name))
                .cloned()
                .collect();
            OrphanManager::save_known(&remaining);
            let recorded = OrphanManager::finish_review(&remaining);
            if !removal_ok || !recorded { exit(1); }
        } else if scope.includes_native() && !dry_run && OrphanManager::load_known().is_none() {
            OrphanManager::save_known(&orphans);
        }
        return;
    }

    let mut pacman_targets = Vec::new();
    let mut aur_targets = Vec::new();

    for p in &updates {
        let cand = match p.candidate.as_ref() {
            Some(c) => c,
            None => continue,
        };

        if cand.is_aur {
            aur_targets.push(p.name.clone());
        } else if p.state == "custom" || p.diverted_from_top {
            pacman_targets.push(format!("{}/{}", cand.repo, p.name));
        } else {
            pacman_targets.push(p.name.clone());
        }
    }
    if let Err(error) = validate_upgrade_targets(&pacman_targets, &aur_targets) {
        eprintln!("{}", format!("Error: {}", error).red());
        exit(1);
    }

    // Merge explicitly requested extras. Anything already covered by the
    // routine update targets stays single; requested forms win the dedup so
    // the executed command matches the Requested box exactly.
    let mut covered: HashSet<String> = pacman_targets
        .iter()
        .map(|target| strip_repo(target).to_string())
        .chain(aur_targets.iter().cloned())
        .collect();
    for extra in &requested_targets.native {
        if covered.insert(strip_repo(extra).to_string()) {
            pacman_targets.push(extra.clone());
        }
    }
    for extra in &requested_targets.aur {
        if covered.insert(extra.clone()) {
            aur_targets.push(extra.clone());
        }
    }

    // Prepare the real native transaction (lock-free, no download, no
    // install) so solver-pulled dependencies, removals, and replacements are
    // approved explicitly instead of surfacing mid-transaction.
    let preview = match crate::progress::operation(crate::progress::Manager::Checks, "Resolving transaction dependencies", || prepare_native_preview(&mut manager, &pacman_targets)) {
        Ok(preview) => preview,
        Err(error) => {
            eprintln!("{}", format!("Error: {error}").red());
            exit(1);
        }
    };
    let mut explicit_names: HashSet<String> = requested_names.clone();
    explicit_names.extend(updates.iter().map(|update| update.name.clone()));
    // An ignored package pulled in as a solver dependency cannot be honored:
    // installing it would void the ignore, skipping it breaks the
    // transaction. Fail closed before approval instead of either.
    let blocked: Vec<&str> = preview
        .adds
        .iter()
        .filter(|pkg| !explicit_names.contains(&pkg.name))
        .map(|pkg| pkg.name.as_str())
        .filter(|name| ignored.contains(*name))
        .collect();
    if !blocked.is_empty() {
        eprintln!(
            "{}",
            format!(
                "Error: {} required by this transaction, but explicitly ignored; remove the matching --ignore/--ignoregroup entry or drop the requesting package.",
                blocked.join(", ")
            )
            .red()
        );
        exit(1);
    }
    let mut hook_expected: std::collections::BTreeMap<String, String> = preview.adds.iter()
        .filter(|p| !ignored.contains(&p.name)).map(|p| (p.name.clone(), p.version.clone())).collect();
    for update in updates.iter().filter(|u| !ignored.contains(&u.name)) {
        if let Some(candidate) = &update.candidate { hook_expected.insert(update.name.clone(), candidate.version.clone()); }
    }
    for update in external_updates.iter().filter(|u| u.runner == "Sources") {
        hook_expected.insert(update.name.clone(), update.version.clone());
    }
    let hook_plan = match crate::hooks::Plan::new(config, crate::hooks::versions(&manager), hook_expected, scope, eligibility.aur_available()) {
        Ok(plan) => plan, Err(error) => { eprintln!("Error: {error}"); exit(1); }
    };
    hook_plan.preview();
    eligibility.preview();
    for provider in &ext_providers {
        let updates: Vec<_> = external_updates.iter().filter(|u| u.runner == provider.name()).cloned().collect();
        if !updates.is_empty() { provider.preview_upgrade(&updates); }
    }
    ui::render_transaction_plan(&res_view, &config.options.helper, &external_updates, Some(ui::PlanDetails {
        preview: &preview,
        explicit_names: &explicit_names,
        requested: &requested_targets.items,
        native_cache: (no_cache && !preview.adds.is_empty()).then(pm::native_cache_usage),
    }));

    // New dependencies are executed as explicit repo-qualified targets so the
    // transaction cannot silently resolve to anything other than the approved
    // plan.
    for dep in preview
        .adds
        .iter()
        .filter(|pkg| pkg.is_new_install() && !explicit_names.contains(&pkg.name))
    {
        let qualified = if dep.repo.is_empty() {
            dep.name.clone()
        } else {
            format!("{}/{}", dep.repo, dep.name)
        };
        if covered.insert(dep.name.clone()) {
            pacman_targets.push(qualified);
        }
    }
    if let Err(error) = validate_upgrade_targets(&pacman_targets, &aur_targets) {
        eprintln!("{}", format!("Error: {}", error).red());
        exit(1);
    }

    if !requested_targets.aur.is_empty() {
        println!(
            "{}",
            format!(
                ":: Note: AUR targets are built by '{}'; it resolves any additional build dependencies at build time.",
                config.options.helper
            )
            .yellow()
        );
    }

    let forwarded_pacman_flags: Vec<&str> = manager_flags
        .iter()
        .filter(|f| {
            let s = f.as_str();
            s != "-y"
                && s != "--refresh"
                && s != "-yy"
                && s != "-u"
                && s != "--sysupgrade"
                && s != "-uu"
                && s != "-c"
                && s != "--clean"
                && s != "--autoremove"
                && s != "-n"
                && s != "--dry-run"
                && s != "--noconfirm"
                && s != "--needed"
                && s != "--no-cache"
                && s != "--clean-cache"
        })
        .map(|s| s.as_str())
        .collect();

    if !aur_targets.is_empty() {
        if let Err(error) = pm::helper_privilege_args(&config.options.helper, &config.options.privilege_tool) {
            eprintln!("Error: {error}"); exit(1);
        }
    }
    let options = pm::InstallOptions { needed: true, noconfirm: true, no_cache,
        forwarded_flags: forwarded_pacman_flags.iter().map(|flag| (*flag).to_string()).collect(), ..Default::default() };
    let mut core_commands = pm::build_install_commands(&pacman_targets, &aur_targets, &config.options.helper, &config.options.privilege_tool, &options, false).0.into_iter();
    let native_command = (!pacman_targets.is_empty()).then(|| core_commands.next().expect("native command"));
    let aur_command = (!aur_targets.is_empty()).then(|| core_commands.next().expect("AUR command"));
    let review_orphans = (autoremove || orphan_list_changed) && !orphans.is_empty();
    let orphan_review = if review_orphans && !noconfirm {
        execution_plan::OrphanReview::Interactive(orphans.len())
    } else {
        let count = noconfirm_orphan_selection(&orphans, autoremove).len();
        if count == 0 { execution_plan::OrphanReview::None } else { execution_plan::OrphanReview::Automatic(count) }
    };
    let execution_plan = match execution_plan::Plan::new(execution_plan::Input {
        native: native_command.clone().map(|command| (command, pacman_targets.len())),
        aur: aur_command.clone().map(|command| (command, aur_targets.len())),
        providers: &ext_providers, updates: &external_updates, orphans: orphan_review,
        native_during_review: review_orphans && !noconfirm && native_command.is_some() && config.options.privilege_tool == "sudo",
        hooks: !hook_plan.is_empty(), aur_hook: hook_plan.has_aur(), no_cache,
    }) { Ok(plan) => plan, Err(error) => { eprintln!("Error: {error}"); exit(1); } };

    if !pacman_targets.is_empty() {
        if let Err(error) = crate::progress::operation(crate::progress::Manager::Checks, "Checking installation space", || crate::preview::check_native_capacity(&preview)) {
            if dry_run {
                eprintln!(":: Dry-run storage check: {error}");
            } else {
                eprintln!("Error: {error}");
                exit(1);
            }
        }
    }
    if !aur_targets.is_empty() && !dry_run {
        if let Err(error) = crate::preview::check_package_root_capacity(0, 0) {
            eprintln!("Error: {error}");
            exit(1);
        }
    }

    let external_scratch: Vec<_> = ext_providers.iter().filter(|p| p.name() != "Sources" && external_updates.iter().any(|u| u.runner == p.name())).map(|p| p.name()).collect();
    if let Some(description) = crate::scratch::describe_managers(external_updates.iter().any(|u| u.runner == "Sources"), !aur_targets.is_empty() || hook_plan.has_aur(), &external_scratch) { println!(":: {description}"); }
    execution_plan.render(dry_run);
    if dry_run {
        if keep_going { println!("Dry-Run: --keep-going skips failed manager discovery and unsupported options, and permits independent Sources after a verified AUR build failure; skipped or failed work keeps the run unsuccessful."); }
        if no_cache {
            println!("{}", "Dry-Run: Conditional cache cleanup after manager success (--no-cache):".bold());
            let has_native = !pacman_targets.is_empty();
            let has_aur = !aur_targets.is_empty() || hook_plan.has_aur();
            if has_native && has_aur {
                println!("  ➔ Purge both native pacman cache and AUR build cache\n");
            } else if has_native {
                println!("  ➔ Purge native pacman cache only ({} pacman -Scc); AUR cache preserved\n", config.options.privilege_tool);
            } else if has_aur {
                println!("  ➔ Purge AUR build cache only ({} -Scc --aur); native pacman cache preserved\n", config.options.helper);
            } else {
                println!("  ➔ No native or AUR cache cleanup in this plan");
            }
            for provider in &ext_providers {
                if external_updates.iter().any(|u| u.runner == provider.name()) {
                    if let Some(action) = provider.cache_command_str() { println!("  ➔ {}: {action}", provider.name()); }
                    else if provider.name() == "Sources" { println!("  ➔ Sources remove their temporary workspaces"); }
                }
            }
        }
        if (autoremove || orphan_list_changed) && !orphans.is_empty() {
            println!("{}", "Dry-Run: Separate orphan cleanup review:".bold());
            ui::render_orphans_summary(&orphans);
        }
        let automatic_orphans = noconfirm_orphan_selection(&orphans, autoremove);
        if !automatic_orphans.is_empty() {
            println!("\n{}", "Dry-Run: Orphan removal only if --autoremove is selected and upgrade succeeds:".bold());
            println!(
                "  ➔ {}\n",
                format!("{} pacman -Rn --noconfirm -- {}", config.options.privilege_tool, automatic_orphans.join(" ")).cyan()
            );
        }
        if eligibility.incomplete() { eligibility.report_incomplete(); exit(2); }
        return;
    }

    let mut conflicts = pm::check_resolved_package_conflicts(&manager, &updates);
    if !requested_targets.native.is_empty() {
        let mut extra_conflicts =
            pm::check_package_conflicts(&manager, &requested_targets.native);
        conflicts.append(&mut extra_conflicts);
        conflicts.sort();
        conflicts.dedup();
    }
    if !conflicts.is_empty() {
        println!("{}", ":: Note: Potential package conflict(s) detected:".yellow().bold());
        for (cand, inst) in &conflicts {
            println!(
                "   - '{}' conflicts with package '{}' (may require replacement)",
                cand.cyan(),
                inst.yellow()
            );
        }
        println!();
    }

    let mut selected_orphans = Vec::new();
    let mut prefetch = None;
    let mut planned_managers = Vec::new();
    if !pacman_targets.is_empty() { planned_managers.push(crate::progress::Manager::Native); }
    if !aur_targets.is_empty() { planned_managers.push(crate::progress::Manager::Aur); }
    planned_managers.extend(external_updates.iter().map(|u| crate::progress::integration_manager(&u.runner)));
    let _pending_plan;

    if !noconfirm {
        if !io::stdin().is_terminal() {
            eprintln!(
                "{}",
                "Error: Cannot prompt for confirmation in non-interactive environment. Use --noconfirm to proceed."
                    .red()
            );
            exit(1);
        }
        let mut resp = String::new();
        if crate::progress::operation(crate::progress::Manager::Review, "Awaiting plan approval", || {
            print!("{}", "Proceed with installation? [Y/n] ".bold());
            io::stdout().flush()?;
            io::stdin().read_line(&mut resp)
        }).is_err() {
            println!("\nAborted.");
            return;
        }
        let r = resp.trim().to_lowercase();
        if !r.is_empty() && r != "y" && r != "yes" {
            println!("Aborted.");
            return;
        }
        _pending_plan = crate::progress::pending(&planned_managers);

        if review_orphans {
            if !pacman_targets.is_empty() && config.options.privilege_tool == "sudo" {
                // Acquire credentials before the TUI takes over the terminal.
                // The child then runs without terminal input or output while
                // the user reviews the projected orphan list.
                match pm::authorize_downloads(config) {
                    Ok(status) if status.success() => {
                        match pm::with_download_progress(&preview, &forwarded_pacman_flags, || crate::progress::start_background(&mut repo_prefetch_command(&pacman_targets, &forwarded_pacman_flags))) {
                            Ok(child) => {
                                println!("{}", ":: Downloading repository packages while you review orphans...".cyan());
                                prefetch = Some(child);
                            }
                            Err(e) => eprintln!("{}", format!(":: Background download could not start: {e}; the upgrade will download normally.").yellow()),
                        }
                    }
                    _ => eprintln!("{}", ":: Could not prepare background downloads; the upgrade will download normally.".yellow()),
                }
            }
            println!("\n{}", ":: Orphan cleanup is optional and separate; selected packages are removed only after a successful upgrade.".cyan());
            ui::render_orphans_summary(&orphans);
            selected_orphans = crate::progress::selection_operation(|| ui::prompt_orphan_selection(&orphans, true));
        }
    } else {
        _pending_plan = crate::progress::pending(&planned_managers);
        selected_orphans = noconfirm_orphan_selection(&orphans, autoremove);
    }

    if (review_orphans && !noconfirm) || !selected_orphans.is_empty() {
        if let Err(error) = OrphanManager::record_selection(&selected_orphans) {
            eprintln!(":: {error}; upgrade aborted before installation.");
            // Let any native download finish and release its lock before exiting.
            if let Some(child) = prefetch.take() { let _ = child.wait(); }
            exit(1);
        }
    }

    let orphan_pending = crate::progress::pending(if selected_orphans.is_empty() { &[] } else { &[crate::progress::Manager::Orphans] });
    let cache_pending = crate::progress::pending(if execution_plan.has_cache_cleanup() { &[crate::progress::Manager::Cache] } else { &[] });

    if let Some(child) = prefetch {
        match child.wait() {
            Ok(status) if status.success() => {
                println!("{}", ":: Repository packages are cached; starting installation.".cyan());
            }
            Err(e) => {
                // Never start a second pacman operation while a failed wait
                // might have left the download-only child holding db.lck.
                eprintln!("{}", format!(":: Background download wait failed ({e}); the upgrade will download normally.").yellow());
            }
            _ => eprintln!("{}", ":: Background download did not finish; the upgrade will download normally.".yellow()),
        }
    }

    let mut applied_updates: Vec<ResolvedPackage> = Vec::new();
    let mut executed_commands: Vec<String> = Vec::new();
    let mut exit_status = Ok(std::process::ExitStatus::default());
    let mut all_success = true;
    let mut continuation_permit = None;
    let mut native_ok = false;
    let mut aur_ok = false;

    // Re-prepare immediately before executing: if the sync databases moved
    // between approval and execution, refuse to run a plan that was not approved.
    if !pacman_targets.is_empty() {
        if let Err(error) = crate::progress::operation(crate::progress::Manager::Checks, "Rechecking installation space", || crate::preview::check_native_capacity(&preview)) {
            eprintln!("Error: {error}");
            exit(1);
        }
        match crate::progress::operation(crate::progress::Manager::Checks, "Revalidating approved transaction", || prepare_native_preview(&mut manager, &pacman_targets)) {
            Ok(fresh) if fresh.signature() == preview.signature() => {}
            Ok(_) => {
                eprintln!("{}", "Error: The native transaction changed after approval (sync databases moved); re-run to review the new plan.".red());
                exit(1);
            }
            Err(error) => {
                eprintln!("{}", format!("Error: Cannot re-verify the approved transaction: {error}").red());
                exit(1);
            }
        }
    }

    let preparation = crate::integrations::preparation::Session::new(&ext_providers, &external_updates, planned_managers);
    if !pacman_targets.is_empty() {
        let mut args = vec!["pacman", "-S", "--needed", "--noconfirm"];
        args.extend(crate::progress::pacman_progress_args());
        args.extend(forwarded_pacman_flags.iter().cloned());
        args.push("--");
        args.extend(pacman_targets.iter().map(|s| s.as_str()));
        let status = pm::with_download_progress(&preview, &forwarded_pacman_flags, || crate::progress::run(
            crate::privilege::command(&config.options.privilege_tool).args(&args),
            crate::progress::Manager::Native,
            "Resolving / downloading repository packages",
            || {},
        ));
        match status {
            Ok(s) if s.success() => {
                for u in &updates {
                    if let Some(cand) = &u.candidate {
                        if !cand.is_aur {
                            applied_updates.push(u.clone());
                        }
                    }
                }
                for item in &requested_targets.items {
                    if item.manager == "aur"
                        || item.status == RequestedStatus::AlreadyCurrent
                        || updates.iter().any(|u| u.name == item.name)
                    {
                        continue;
                    }
                    if let Some(entry) = preview.adds.iter().find(|p| p.name == item.name) {
                        applied_updates.push(requested_resolved_package(item, entry));
                    }
                }
                native_ok = true;
                crate::progress::completed(crate::progress::Manager::Native);
                let dependencies: Vec<_> = crate::preview::partition_new_dependencies(&preview, &explicit_names).into_iter().map(|p| p.name.as_str()).collect();
                if !dependencies.is_empty() {
                    println!(":: Recording newly installed packages as dependencies...");
                    match pm::mark_dependencies(config, &dependencies) {
                        Ok(status) if status.success() => {}
                        _ => eprintln!(":: Warning: Packages installed, but dependency install reasons could not be updated. Review them with pacman -Qi."),
                    }
                }
                executed_commands.push(native_command.clone().expect("native command"));
            }
            Ok(s) => {
                exit_status = Ok(s);
                all_success = false;
                eprintln!(":: Native transaction failed. Some packages or files may already have changed; the system is not guaranteed to be unchanged. Remaining managers will be skipped.");
            }
            Err(e) => {
                exit_status = Err(e);
                all_success = false;
            }
        }
    }

    if all_success && !aur_targets.is_empty() {
        let options = pm::InstallOptions { needed: true, noconfirm: true, no_cache,
            forwarded_flags: forwarded_pacman_flags.iter().map(|s| (*s).to_owned()).collect(), ..Default::default() };
        let report = pm::execute_aur(config, &aur_targets, &options, pm::AurOperation::Install, keep_going);
        let status = report.map(|report| {
            if !report.status.success() && keep_going {
                match report.continuation {
                    Ok(permit) => { println!(":: AUR build failed without observed native changes; checking independent Sources (--keep-going)."); continuation_permit = Some(permit); }
                    Err(reason) => eprintln!(":: Continuation disabled: {reason}"),
                }
            }
            report.status
        });
        match status {
            Ok(s) if s.success() => {
                aur_ok = true;
                crate::progress::completed(crate::progress::Manager::Aur);
                for u in &updates {
                    if let Some(cand) = &u.candidate {
                        if cand.is_aur {
                            applied_updates.push(u.clone());
                        }
                    }
                }
                for item in &requested_targets.items {
                    if item.manager != "aur"
                        || item.status == RequestedStatus::AlreadyCurrent
                        || updates.iter().any(|u| u.name == item.name)
                    {
                        continue;
                    }
                    applied_updates.push(requested_aur_resolved_package(item));
                }
                executed_commands.push(aur_command.clone().expect("AUR command"));
            }
            Ok(s) => {
                exit_status = Ok(s);
                all_success = false;
            }
            Err(e) => {
                exit_status = Err(e);
                all_success = false;
            }
        }
    }

    let mut external_outcomes = Vec::new();
    if all_success && !external_updates.is_empty() {
        let outcomes = IntegrationsManager::execute_upgrades(&ext_providers, &external_updates, Some(&preparation));
        for (p_name, cmd_str, success) in outcomes {
            external_outcomes.push((p_name, success));
            if success {
                executed_commands.push(cmd_str);
            } else {
                all_success = false;
            }
        }
    } else if let Some(mut permit) = continuation_permit.filter(|_| !external_updates.is_empty()) {
        for (cmd, success) in IntegrationsManager::execute_after_aur_failure(&ext_providers, &external_updates, &mut permit, Some(&preparation)) {
            if success { executed_commands.push(cmd); }
        }
        println!(":: The AUR stage failed; this run remains unsuccessful. Orphan cleanup is skipped.");
    } else if !external_updates.is_empty() {
        println!(":: External upgrades skipped because an earlier package transaction failed. Download verification does not mean installation succeeded.");
    }

    preparation.finish();
    if !all_success { crate::progress::stop_pending(); }
    crate::sources::clear_prefetched();

    // Journal any packages that were successfully applied to disk.
    // Packages the user ignored are skipped by pacman at execution, so they
    // must not be recorded as applied (or rollback would reason over a
    // transaction that never happened).
    let ignored = ignored_names(extra_flags);
    applied_updates.retain(|u| !ignored.contains(&u.name));
    if !applied_updates.is_empty() {
        let mut tx_packages: Vec<serde_json::Value> = applied_updates
            .iter()
            .map(|u| {
                let cand = u.candidate.as_ref();
                // Rollback restores `from_version` entries, so never-installed
                // packages must omit the key instead of recording an empty one.
                let mut entry = serde_json::Map::new();
                entry.insert("name".to_string(), u.name.clone().into());
                entry.insert(
                    "from_repo".to_string(),
                    (if u.installed_db.is_empty() {
                        "unknown"
                    } else {
                        u.installed_db.as_str()
                    })
                    .to_string()
                    .into(),
                );
                if !u.installed_ver.is_empty() {
                    entry.insert("from_version".to_string(), u.installed_ver.clone().into());
                }
                entry.insert(
                    "to_repo".to_string(),
                    cand.map(|c| c.repo.as_str()).unwrap_or("unknown").to_string().into(),
                );
                entry.insert(
                    "to_version".to_string(),
                    cand.map(|c| c.version.as_str()).unwrap_or("unknown").to_string().into(),
                );
                entry.insert("state".to_string(), u.state.clone().into());
                serde_json::Value::Object(entry)
            })
            .collect();
        if native_ok {
            // Record solver-pulled dependencies that were never requested so
            // history shows the full approved plan.
            for dep in preview.adds.iter().filter(|pkg| !explicit_names.contains(&pkg.name)) {
                let mut entry = serde_json::json!({
                    "name": dep.name,
                    "to_repo": dep.repo,
                    "to_version": dep.version,
                    "state": "dependency",
                });
                if let Some(old) = &dep.old_version { entry["from_version"] = old.clone().into(); }
                tx_packages.push(entry);
            }
        }
        let cmd_to_record = if executed_commands.is_empty() {
            execution_plan.summary()
        } else {
            executed_commands.join("\n")
        };
        let _ = crate::progress::operation(crate::progress::Manager::Record, "Saving upgrade history", || TransactionJournal::record_transaction("upgrade", tx_packages, &cmd_to_record));

    }

    let hook_outcome = hook_plan.run(config, all_success, noconfirm);
    all_success &= hook_outcome.success;
    all_success &= !eligibility.incomplete();
    if eligibility.incomplete() && scope.includes_native() && (config.features.smart_orphans || autoremove) {
        println!(":: Orphan cleanup skipped because the upgrade plan is incomplete.");
    }

    if all_success && scope.includes_native() && (config.features.smart_orphans || autoremove) {
        // Projected orphans may still be required, or no longer installed,
        // after the real transaction.
        match AlpmManager::with_repo_order(&config.repo_order) {
            Ok(fresh_manager) => {
                let actual_orphans = crate::progress::operation(crate::progress::Manager::Checks, "Rechecking orphan dependencies", || fresh_manager.get_orphans(&[]));
                let safe_selection = revalidate_orphan_selection(&selected_orphans, &actual_orphans, noconfirm);
                if safe_selection.len() != selected_orphans.len() {
                    eprintln!("{}", ":: Some projected orphans are still required or were already removed; skipping those selections.".yellow());
                }
                let result = OrphanManager::execute_removal(&safe_selection, &config.options.privilege_tool);
                all_success &= result.is_ok();
                let removed = result.unwrap_or_else(|error| { eprintln!(":: Warning: {error}"); Vec::new() });
                let remaining: Vec<OrphanPackage> = actual_orphans
                    .into_iter()
                    .filter(|o| !removed.contains(&o.name))
                    .collect();
                OrphanManager::save_known(&remaining);
                all_success &= OrphanManager::finish_review(&remaining);
            }
            Err(e) => eprintln!("{}", format!(":: Cannot recheck orphans after upgrade; skipping cleanup: {e}").yellow()),
        }
    } else if !all_success && !selected_orphans.is_empty() {
        println!("\n:: Orphan cleanup skipped because the upgrade failed; pending cleanup will be offered again on the next full upgrade.");
    }

    drop(orphan_pending);
    let aur_cleanup = match (if !aur_targets.is_empty() && (native_ok || pacman_targets.is_empty()) { Some(aur_ok) } else { None }, hook_outcome.aur) {
        (Some(initial), Some(hook)) => Some(initial && hook),
        (initial, hook) => initial.or(hook),
    };
    let cleanup_ok = pm::transaction_cache_cleanup(config, no_cache,
        (!pacman_targets.is_empty()).then_some(native_ok),
        aur_cleanup);
    let external_cleanup_ok = IntegrationsManager::transaction_cache_cleanup(&ext_providers, &external_outcomes, no_cache);
    drop(cache_pending);

    if !applied_updates.is_empty() {
        restart::RestartInspector::print_restart_advisory(&applied_updates, &config.options.privilege_tool);
    }
    all_success &= cleanup_ok && external_cleanup_ok;
    if !all_success {
        if eligibility.incomplete() { eligibility.report_incomplete(); }
        match exit_status {
            Ok(s) => exit(s.code().filter(|code| *code != 0).unwrap_or(if eligibility.incomplete() { 2 } else { 1 })),
            Err(e) => {
                eprintln!("{}", format!("Execution failed: {}", e).red());
                exit(1);
            }
        }
    }
}

fn noconfirm_orphan_selection(orphans: &[OrphanPackage], autoremove: bool) -> Vec<String> {
    if !autoremove {
        return Vec::new();
    }
    orphans.iter().filter(|o| o.is_pure()).map(|o| o.name.clone()).collect()
}

fn repo_prefetch_command(targets: &[String], forwarded_flags: &[&str]) -> Command {
    let mut command = crate::privilege::command("sudo");
    command.args(["-n", "pacman", "-Sw", "--needed", "--noconfirm"]);
    command.args(forwarded_flags);
    command.arg("--").args(targets);
    command.stdin(Stdio::inherit()).stdout(Stdio::null()).stderr(Stdio::null());
    command
}

fn revalidate_orphan_selection(
    selected: &[String],
    actual: &[OrphanPackage],
    noconfirm: bool,
) -> Vec<String> {
    selected.iter().filter(|name| {
        actual.iter().any(|orphan| orphan.name == **name && (!noconfirm || orphan.is_pure()))
    }).cloned().collect()
}

pub fn parse_install_flags(flags: &[String]) -> pm::InstallOptions {
    let manager_flags = crate::runtime::manager_flags(flags);
    let flags = manager_flags.as_slice();
    let channel = flags.iter().enumerate().find_map(|(index, flag)| {
        let value = flag.strip_prefix("--channel=").or_else(|| (flag == "--channel").then(|| flags.get(index + 1)).flatten().map(String::as_str))?;
        match value {
            "stable" => Some(crate::sources::Channel::Stable),
            "alpha" => Some(crate::sources::Channel::Alpha),
            "beta" => Some(crate::sources::Channel::Beta),
            "prerelease" => Some(crate::sources::Channel::Prerelease),
            _ => None,
        }
    });
    let dry_run = flags.iter().any(|f| f == "-n" || f == "--dry-run")
        || flags
            .iter()
            .any(|f| f.starts_with('-') && !f.starts_with("--") && f[1..].contains('n'));
    let refresh = flags.iter().any(|f| f == "-y" || f == "--refresh" || f == "-yy")
        || flags
            .iter()
            .any(|f| f.starts_with('-') && !f.starts_with("--") && f[1..].contains('y'));
    let sysupgrade = flags.iter().any(|f| f == "-u" || f == "--sysupgrade" || f == "-uu")
        || flags
            .iter()
            .any(|f| f.starts_with('-') && !f.starts_with("--") && f[1..].contains('u'));
    let noconfirm = flags.iter().any(|f| f == "--noconfirm");
    let no_cache = flags.iter().any(|f| f == "--no-cache" || f == "--clean-cache");
    let needed = true;

    let forwarded_flags: Vec<String> = flags
        .iter()
        .enumerate()
        .filter(|(index, f)| {
            let s = f.as_str();
            if *index > 0 && flags[*index - 1] == "--channel" { return false; }
            if s == "--refresh"
                || s == "--sysupgrade"
                || s == "--dry-run"
                || s == "--noconfirm"
                || s == "--needed" || s == "--channel" || s.starts_with("--channel=")
                || s == "--no-cache" || s == "--clean-cache"
            {
                return false;
            }
            if s.starts_with('-') && !s.starts_with("--") {
                let chars = s[1..].chars();
                if chars.clone().all(|c| c == 'y' || c == 'u' || c == 'n') {
                    return false;
                }
            }
            true
        })
        .map(|(_, f)| f.clone())
        .collect();

    pm::InstallOptions {
        needed,
        noconfirm,
        refresh,
        sysupgrade,
        dry_run,
        forwarded_flags,
        channel,
        no_cache,
    }
}

pub fn cmd_install(mut config: Config, targets: &[String], flags: &[String]) {
    if targets.is_empty() {
        eprintln!("{}", "Error: No package targets specified.".red());
        eprintln!("Usage: pacpin -S [flags] [repo/]package ...");
        exit(1);
    }

    check_pacman_lock();
    print_banner();

    if let Err(error) = validate_channel_flags(flags) {
        eprintln!("{}", format!("Error: {error}").red());
        exit(1);
    }
    let options = parse_install_flags(flags);

    match pm::install(&mut config, targets, &options) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{}", format!("Error: {}", e).red());
            exit(1);
        }
    }
}

fn validate_channel_flags(flags: &[String]) -> Result<(), String> {
    let mut values = Vec::new();
    for (index, flag) in flags.iter().enumerate() {
        if let Some(value) = flag.strip_prefix("--channel=") {
            values.push(value);
        } else if flag == "--channel" {
            values.push(flags.get(index + 1).map(String::as_str).ok_or("--channel requires stable, alpha, beta, or prerelease")?);
        }
    }
    if values.len() > 1 { return Err("--channel may only be specified once".into()); }
    if values.iter().any(|value| !matches!(*value, "stable" | "alpha" | "beta" | "prerelease")) {
        return Err("--channel must be stable, alpha, beta, or prerelease".into());
    }
    Ok(())
}
