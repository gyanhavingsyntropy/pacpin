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

use std::cmp::Ordering;
use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::process::{exit, Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use alpm::vercmp;
use colored::Colorize;

use crate::aur;
use crate::commands::check_pacman_lock;
use crate::config::Config;
use crate::db::{AlpmManager, OrphanPackage};
use crate::integrations::{self, IntegrationsManager};
use crate::journal::TransactionJournal;
use crate::orphans::OrphanManager;
use crate::pm;
use crate::preview::{
    RequestedItem, RequestedStatus, prepare_native_preview, strip_repo,
};
use crate::resolver::{ResolveResult, ResolvedPackage, ResolverEngine};
use crate::restart;
use crate::ui::{self, print_banner};

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
    let mut provider_config = config.clone();
    if scope != crate::runtime::UpgradeScope::All { provider_config.features.integrations = false; }
    if scope == crate::runtime::UpgradeScope::Aur { provider_config.sources.clear(); }
    let ext_providers = IntegrationsManager::get_active_providers(&provider_config)
        .into_iter().filter(|p| scope.includes_runner(p.name())).collect::<Vec<_>>();
    if !scope.includes_native() && refresh {
        println!(":: Scoped upgrade: native repository databases are not refreshed. AUR builds may still require native build dependencies.");
    }

    if refresh && !dry_run && scope.includes_native() {
        println!(
            ":: Refreshing package databases ({} pacman -Sy)...",
            config.options.privilege_tool
        );

        let ext_refresh_providers = ext_providers.clone();
        let ext_refresh_handle = if !ext_refresh_providers.is_empty() {
            Some(thread::spawn(move || {
                IntegrationsManager::refresh_all_parallel(&ext_refresh_providers);
            }))
        } else {
            None
        };

        let status = crate::privilege::command(&config.options.privilege_tool).args(["pacman", "-Sy"]).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
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
                eprintln!("{}", format!("Failed to run {} pacman: {}", config.options.privilege_tool, e).red());
                exit(1);
            }
        }

        if let Some(h) = ext_refresh_handle {
            let _ = h.join();
        }
    }

    let ext_providers_for_check = ext_providers.clone();
    let ext_handle = thread::spawn(move || {
        IntegrationsManager::check_updates_parallel(&ext_providers_for_check)
    });

    let mut manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };
    for warn in if scope == crate::runtime::UpgradeScope::Sources { Vec::new() } else { manager.verify_syncdbs() } {
        eprintln!("{}", format!(":: Warning: {}", warn).yellow());
    }
    let resolver = ResolverEngine::new(&manager);
    let mut res = if scope == crate::runtime::UpgradeScope::Sources {
        ResolveResult { installed_count: manager.handle().localdb().pkgs().len(), repos: Vec::new(), packages: std::collections::HashMap::new(), updates: Vec::new(), held_packages: Vec::new(), unresolved_pins: Vec::new() }
    } else { resolver.resolve_all(config) };
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
    let orphans = if scope.includes_native() && (config.features.smart_orphans || autoremove) {
        manager.get_orphans(&res.updates)
    } else {
        Vec::new()
    };
    let mut external_updates = ext_handle.join().unwrap_or_default();
    external_updates.retain(|update| update.runner != "Sources" || !crate::sources::ignored(&update.name, extra_flags));

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
    let requested_targets = match resolve_requested_targets(config, &manager, &res, requested, &ignored) {
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
    let requested_names: HashSet<String> = requested_targets
        .items
        .iter()
        .map(|item| item.name.clone())
        .collect();
    let res_view = ResolveResult {
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
    let orphan_list_changed = if scope.includes_native() && config.features.smart_orphans {
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
    let preview = match prepare_native_preview(&mut manager, &pacman_targets) {
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
    let hook_plan = match crate::hooks::Plan::new(config, crate::hooks::versions(&manager), hook_expected, scope) {
        Ok(plan) => plan, Err(error) => { eprintln!("Error: {error}"); exit(1); }
    };
    hook_plan.preview();
    ui::render_transaction_plan(&res_view, &config.options.helper, &external_updates, Some(ui::PlanDetails {
        preview: &preview,
        explicit_names: &explicit_names,
        requested: &requested_targets.items,
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

    let no_cache = config.options.no_cache
        || extra_flags.iter().any(|f| f == "--no-cache" || f == "--clean-cache");

    let manager_flags = crate::runtime::manager_flags(extra_flags);
    if !aur_targets.is_empty() {
        if let Err(error) = pm::validate_aur_scratch(config, &manager_flags) { eprintln!("Error: {error}"); exit(2); }
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

    let mut command_strs = Vec::new();
    let helper_flags = if aur_targets.is_empty() { Vec::new() } else {
        match pm::helper_privilege_args(&config.options.helper, &config.options.privilege_tool) {
            Ok(flags) => flags,
            Err(error) => { eprintln!("Error: {error}"); exit(1); }
        }
    };

    if !pacman_targets.is_empty() {
        let mut p_cmd = vec![config.options.privilege_tool.as_str(), "pacman", "-S", "--needed", "--noconfirm"];
        p_cmd.extend(forwarded_pacman_flags.iter().copied());
        p_cmd.push("--");
        let mut s = p_cmd.join(" ");
        s.push(' ');
        s.push_str(&pacman_targets.join(" "));
        command_strs.push(s);
    }

    if !aur_targets.is_empty() {
        let helper = &config.options.helper;
        let mut a_cmd = vec![helper.as_str(), "-S", "--needed", "--aur", "--noconfirm"];
        a_cmd.extend(helper_flags.iter().copied());
        if no_cache {
            a_cmd.push("--cleanafter");
        }
        a_cmd.extend(forwarded_pacman_flags.iter().copied());
        a_cmd.push("--");
        let mut s = a_cmd.join(" ");
        s.push(' ');
        s.push_str(&aur_targets.join(" "));
        command_strs.push(s);
    }

    for p in &ext_providers {
        let p_updates: Vec<integrations::ExternalUpdate> = external_updates
            .iter()
            .filter(|u| u.runner == p.name())
            .cloned()
            .collect();
        if !p_updates.is_empty() {
            command_strs.push(p.upgrade_command_str(&p_updates));
        }
    }

    let full_cmd = command_strs.join(" && ");

    if !pacman_targets.is_empty() {
        if let Err(error) = crate::preview::check_native_capacity(&preview) {
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

    if dry_run {
        if keep_going { println!("Dry-Run: --keep-going permits independent Sources only after a verified AUR build failure; the overall run still fails."); }
        if let Some(description) = crate::scratch::describe_plan(external_updates.iter().any(|u| u.runner == "Sources"), !aur_targets.is_empty() || hook_plan.has_aur()) { println!("Dry-Run: {description}"); }
        println!("{}", "Dry-Run: Synthesized Upgrade Commands:".bold());
        println!("  ➔ {}\n", full_cmd.cyan());
        if no_cache {
            println!("{}", "Dry-Run: Cache Cleanup Action (--no-cache):".bold());
            let has_native = !pacman_targets.is_empty();
            let has_aur = !aur_targets.is_empty() || hook_plan.has_aur();
            if has_native && has_aur {
                println!("  ➔ Purge both native pacman cache and AUR build cache\n");
            } else if has_native {
                println!("  ➔ Purge native pacman cache only ({} pacman -Scc); AUR cache preserved\n", config.options.privilege_tool);
            } else if has_aur {
                println!("  ➔ Purge AUR build cache only ({} -Scc --aur); native pacman cache preserved\n", config.options.helper);
            } else {
                println!("  ➔ No pacman or AUR caches to purge; sources clean their own temporary workspace\n");
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

    println!("{}", ":: Synthesized Upgrade Command:".cyan());
    println!("  {}\n", full_cmd.bold());

    let review_orphans = (autoremove || orphan_list_changed) && !orphans.is_empty();
    let mut selected_orphans = Vec::new();
    let mut prefetch = None;

    if !noconfirm {
        if !io::stdin().is_terminal() {
            eprintln!(
                "{}",
                "Error: Cannot prompt for confirmation in non-interactive environment. Use --noconfirm to proceed."
                    .red()
            );
            exit(1);
        }
        print!("{}", "Proceed with installation? [Y/n] ".bold());
        io::stdout().flush().unwrap();
        let mut resp = String::new();
        if io::stdin().read_line(&mut resp).is_err() {
            println!("\nAborted.");
            return;
        }
        let r = resp.trim().to_lowercase();
        if !r.is_empty() && r != "y" && r != "yes" {
            println!("Aborted.");
            return;
        }

        if review_orphans {
            if !pacman_targets.is_empty() && config.options.privilege_tool == "sudo" {
                // Acquire credentials before the TUI takes over the terminal.
                // The child then runs without terminal input or output while
                // the user reviews the projected orphan list.
                match crate::privilege::command(&config.options.privilege_tool).arg("-v").status() {
                    Ok(status) if status.success() => {
                        match crate::progress::start_background(&mut repo_prefetch_command(&pacman_targets, &forwarded_pacman_flags)) {
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

    // Wait until native downloads finish, so sources do not compete for the
    // connection. Sources-only transactions download in the foreground.
    let can_prefetch = !dry_run && IntegrationsManager::has_prefetch(&ext_providers, &external_updates)
        && (!pacman_targets.is_empty() || !aur_targets.is_empty());
    let mut ext_prefetch_handle = None;
    if can_prefetch { crate::progress::queued_sources(); }

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
        if let Err(error) = crate::preview::check_native_capacity(&preview) {
            eprintln!("Error: {error}");
            exit(1);
        }
        match prepare_native_preview(&mut manager, &pacman_targets) {
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

    if !pacman_targets.is_empty() {
        let mut args = vec!["pacman", "-S", "--needed", "--noconfirm"];
        args.extend(crate::progress::pacman_progress_args());
        args.extend(forwarded_pacman_flags.iter().cloned());
        args.push("--");
        args.extend(pacman_targets.iter().map(|s| s.as_str()));
        let status = crate::progress::run(
            crate::privilege::command(&config.options.privilege_tool).args(&args),
            crate::progress::Manager::Native,
            "Resolving / downloading repository packages",
            || {
                if can_prefetch && ext_prefetch_handle.is_none() {
                    ext_prefetch_handle = IntegrationsManager::spawn_prefetch(&ext_providers, &external_updates);
                }
            },
        );
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
                    match crate::privilege::command(&config.options.privilege_tool).args(["pacman", "-D", "--asdeps", "--"]).args(&dependencies).status() {
                        Ok(status) if status.success() => {}
                        _ => eprintln!(":: Warning: Packages installed, but dependency install reasons could not be updated. Review them with pacman -Qi."),
                    }
                }
                let mut cmd_parts = vec![config.options.privilege_tool.as_str(), "pacman", "-S", "--needed", "--noconfirm"];
                cmd_parts.extend(forwarded_pacman_flags.iter().cloned());
                cmd_parts.push("--");
                let mut full_cmd_s = cmd_parts.join(" ");
                full_cmd_s.push(' ');
                full_cmd_s.push_str(&pacman_targets.join(" "));
                executed_commands.push(full_cmd_s);
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
        // If native pacman had no transaction/stage output, start here after
        // it exits successfully; AUR-only transactions can overlap immediately.
        if can_prefetch && ext_prefetch_handle.is_none() {
            ext_prefetch_handle = IntegrationsManager::spawn_prefetch(&ext_providers, &external_updates);
        }
        let helper = &config.options.helper;
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
                let mut cmd_parts = vec![helper.as_str(), "-S", "--needed", "--aur", "--noconfirm"];
                cmd_parts.extend(helper_flags.iter().copied());
                if no_cache {
                    cmd_parts.push("--cleanafter");
                }
                cmd_parts.extend(forwarded_pacman_flags.iter().cloned());
                cmd_parts.push("--");
                let mut full_cmd_s = cmd_parts.join(" ");
                full_cmd_s.push(' ');
                full_cmd_s.push_str(&aur_targets.join(" "));
                executed_commands.push(full_cmd_s);
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

    if let Some(h) = ext_prefetch_handle {
        if !h.is_finished() {
            println!("{}", ":: Waiting for background source download to finish...".cyan());
            crate::progress::wait_for_sources(&h);
        }
        let _ = h.join();
        crate::progress::finish_sources();
    } else if can_prefetch {
        // A failed native download never started prefetch. Drop its queued
        // row without reporting work that remained waiting after the failure.
        crate::progress::clear_sources();
    }

    if all_success && !external_updates.is_empty() {
        let outcomes = IntegrationsManager::execute_upgrades(&ext_providers, &external_updates);
        for (_p_name, cmd_str, success) in outcomes {
            if success {
                executed_commands.push(cmd_str);
            } else {
                all_success = false;
            }
        }
    } else if let Some(mut permit) = continuation_permit.filter(|_| !external_updates.is_empty()) {
        for (cmd, success) in IntegrationsManager::execute_after_aur_failure(&ext_providers, &external_updates, &mut permit) {
            if success { executed_commands.push(cmd); }
        }
        println!(":: The AUR stage failed; this run remains unsuccessful. Orphan cleanup is skipped.");
    } else if !external_updates.is_empty() {
        println!(":: External upgrades skipped because an earlier package transaction failed. Download verification does not mean installation succeeded.");
    }

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
            full_cmd.clone()
        } else {
            executed_commands.join(" && ")
        };
        let _ = TransactionJournal::record_transaction("upgrade", tx_packages, &cmd_to_record);

    }

    let hook_outcome = hook_plan.run(config, all_success, noconfirm);
    all_success &= hook_outcome.success;

    if all_success && scope.includes_native() && (config.features.smart_orphans || autoremove) {
        // Projected orphans may still be required, or no longer installed,
        // after the real transaction.
        match AlpmManager::with_repo_order(&config.repo_order) {
            Ok(fresh_manager) => {
                let actual_orphans = fresh_manager.get_orphans(&[]);
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

    let aur_cleanup = match (if !aur_targets.is_empty() && (native_ok || pacman_targets.is_empty()) { Some(aur_ok) } else { None }, hook_outcome.aur) {
        (Some(initial), Some(hook)) => Some(initial && hook),
        (initial, hook) => initial.or(hook),
    };
    let cleanup_ok = pm::transaction_cache_cleanup(config, no_cache,
        (!pacman_targets.is_empty()).then_some(native_ok),
        aur_cleanup);

    if !applied_updates.is_empty() {
        restart::RestartInspector::print_restart_advisory(&applied_updates, &config.options.privilege_tool);
    }
    all_success &= cleanup_ok;
    if !all_success {
        match exit_status {
            Ok(s) => exit(s.code().filter(|code| *code != 0).unwrap_or(1)),
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

fn validate_upgrade_targets(pacman_targets: &[String], aur_targets: &[String]) -> Result<(), String> {
    if let Some(target) = pacman_targets.iter().find(|t| !crate::utils::is_safe_install_target(t)) {
        return Err(format!("Refusing unsafe pacman upgrade target '{}'.", target));
    }
    if let Some(target) = aur_targets.iter().find(|t| !crate::utils::is_safe_pkg_name(t)) {
        return Err(format!("Refusing unsafe AUR upgrade target '{}'.", target));
    }
    Ok(())
}

/// Builds the journal/restart record for an explicitly requested native
/// package from its approved preview entry.
fn requested_resolved_package(
    item: &RequestedItem,
    entry: &crate::preview::PreviewPackage,
) -> ResolvedPackage {
    ResolvedPackage {
        name: item.name.clone(),
        state: "explicit".to_string(),
        pinned_repo: None,
        installed_ver: item.installed_version.clone().unwrap_or_default(),
        installed_size: entry.old_isize,
        installed_db: String::new(),
        candidate: Some(crate::resolver::CandidatePackage {
            name: item.name.clone(),
            version: entry.version.clone(),
            repo: entry.repo.clone(),
            base: item.name.clone(),
            csize: 0,
            isize: entry.new_isize,
            desc: String::new(),
            builddate: 0,
            is_aur: false,
            depends: Vec::new(),
        }),
        natural_top_repo: None,
        diverted_from_top: false,
        needs_update: true,
        update_type: "explicit".to_string(),
        is_downgrade: item.status == RequestedStatus::Downgrade,
        net_delta: Some(entry.net_delta()),
        held: false,
        hold_reason: String::new(),
    }
}

/// Builds the journal record for an explicitly requested AUR package. Build
/// sizes are unknown until the helper builds them.
fn requested_aur_resolved_package(item: &RequestedItem) -> ResolvedPackage {
    ResolvedPackage {
        name: item.name.clone(),
        state: "explicit".to_string(),
        pinned_repo: None,
        installed_ver: item.installed_version.clone().unwrap_or_default(),
        installed_size: 0,
        installed_db: String::new(),
        candidate: Some(crate::resolver::CandidatePackage {
            name: item.name.clone(),
            version: item.candidate_version.clone(),
            repo: "aur".to_string(),
            base: item.name.clone(),
            csize: 0,
            isize: 0,
            desc: String::new(),
            builddate: 0,
            is_aur: true,
            depends: Vec::new(),
        }),
        natural_top_repo: None,
        diverted_from_top: false,
        needs_update: true,
        update_type: "explicit".to_string(),
        is_downgrade: item.status == RequestedStatus::Downgrade,
        net_delta: None,
        held: false,
        hold_reason: String::new(),
    }
}

struct RequestedTargets {
    items: Vec<RequestedItem>,
    native: Vec<String>,
    aur: Vec<String>,
}

fn requested_status(installed: Option<&str>, candidate: &str) -> RequestedStatus {
    match installed {
        None => RequestedStatus::NewInstall,
        Some(current) => match vercmp(current, candidate) {
            Ordering::Less => RequestedStatus::Upgrade,
            Ordering::Greater => RequestedStatus::Downgrade,
            Ordering::Equal => RequestedStatus::AlreadyCurrent,
        },
    }
}

/// True when `--ignore <pkg>` / `--ignore=<pkg>` explicitly names this package.
/// All package names named by `--ignore <pkg>` / `--ignore=<pkg>` (comma
/// separated) plus `--ignoregroup` (expanded to current group members).
/// Pacman skips these at execution, so they must never be recorded as applied
/// afterwards.
fn ignored_names(extra_flags: &[String]) -> HashSet<String> {
    let mut ignored = HashSet::new();
    let mut iter = extra_flags.iter();
    while let Some(flag) = iter.next() {
        if let Some(list) = flag.strip_prefix("--ignore=") {
            ignored.extend(list.split(',').map(str::to_string));
        } else if flag == "--ignore" {
            if let Some(next) = iter.next() {
                ignored.extend(next.split(',').map(str::to_string));
            }
        }
    }
    ignored.retain(|name| !name.is_empty());
    ignored
}

/// Expands `--ignoregroup` entries to current sync-group members. Ignored
/// updates are excluded from the plan entirely: pacman answers "Install
/// anyway?" affirmatively for explicitly listed targets under --noconfirm,
/// so merely forwarding the flags would silently void ignores.
fn collect_ignored_names(manager: &AlpmManager, extra_flags: &[String]) -> HashSet<String> {
    let mut ignored = ignored_names(extra_flags);
    let mut groups = Vec::new();
    let mut iter = extra_flags.iter();
    while let Some(flag) = iter.next() {
        if let Some(list) = flag.strip_prefix("--ignoregroup=") {
            groups.extend(list.split(',').map(str::to_string));
        } else if flag == "--ignoregroup" {
            if let Some(next) = iter.next() {
                groups.extend(next.split(',').map(str::to_string));
            }
        }
    }
    for group in groups.iter().filter(|g| !g.is_empty()) {
        for db in manager.handle().syncdbs() {
            if let Ok(g) = db.group(group.as_str()) {
                ignored.extend(g.packages().into_iter().map(|p| p.name().to_string()));
            }
        }
    }
    ignored
}

fn check_requested_delay(name: &str, builddate: i64, config: &Config, now_epoch: i64) -> Result<(), String> {
    if !config.features.stability_delays || builddate <= 0 {
        return Ok(());
    }
    let glob_delays = ResolverEngine::compile_glob_delays(&config.delay);
    if let Some(days) = ResolverEngine::match_delay_days(name, &config.delay, &glob_delays) {
        let age_days = now_epoch.saturating_sub(builddate) / 86400;
        if age_days < days as i64 {
            return Err(format!(
                "'{name}' is held by a stability delay ({days} days; {} remaining); it cannot be pulled into this upgrade.",
                (days as i64 - age_days).max(0)
            ));
        }
    }
    Ok(())
}

/// Classifies explicitly requested upgrade targets (`pin -Syu <pkg...>`)
/// into native repo targets, AUR targets, and display items. Source-owned and
/// direct upstream targets are rejected with guidance instead of being
/// silently ignored: they are already tracked by `pin -Syu`.
fn resolve_requested_targets(
    config: &Config,
    manager: &AlpmManager,
    res: &ResolveResult,
    requested: &[String],
    ignored: &HashSet<String>,
) -> Result<RequestedTargets, String> {
    let mut out = RequestedTargets {
        items: Vec::new(),
        native: Vec::new(),
        aur: Vec::new(),
    };
    if requested.is_empty() {
        return Ok(out);
    }
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let held_by_name = |name: &str| res.held_packages.iter().any(|held| held.name == name);
    let installed_version = |name: &str| {
        manager
            .handle()
            .localdb()
            .pkg(name)
            .ok()
            .map(|pkg| pkg.version().to_string())
    };
    let pins = if config.features.pinning {
        config.pins.clone()
    } else {
        std::collections::BTreeMap::new()
    };
    let glob_pins = ResolverEngine::compile_glob_pins(&pins);

    for target in requested {
        // Silently repeated targets are shown and executed once; the first
        // spelling wins so the Requested box never lists duplicates.
        if out.items.iter().any(|item| item.name == strip_repo(target)) {
            continue;
        }
        if crate::sources::is_direct_syntax(target) {            return Err(format!(
                "Direct upstream source '{target}' cannot be combined with a system upgrade; it is already tracked by 'pin -Syu', or reinstall it separately with 'pin -S {target}'."
            ));
        }
        if let Some((head, tail)) = target.split_once('/') {
            if config.sources.contains_key(head) {
                return Err(format!(
                    "Upstream source '{target}' cannot be combined with a system upgrade; it is already tracked by 'pin -Syu', or reinstall it separately with 'pin -S {target}'."
                ));
            }
            if !crate::utils::is_safe_install_target(target) {
                return Err(format!("Refusing unsafe package target '{target}'."));
            }
            if head.eq_ignore_ascii_case("aur") {
                if !crate::utils::is_safe_pkg_name(tail) {
                    return Err(format!("Refusing unsafe AUR upgrade target '{tail}'."));
                }
                let found = aur::query_aur(std::slice::from_ref(&tail.to_string()));
                let item = found.get(tail).ok_or_else(|| {
                    format!("'{target}' was not found in the AUR.")
                })?;
                if held_by_name(tail) {
                    return Err(format!("'{tail}' is currently held back and cannot be pulled into this upgrade."));
                }
                if ignored.contains(tail) {
                    return Err(format!("'{tail}' is explicitly ignored; remove the matching --ignore/--ignoregroup entry to include it in this upgrade."));
                }
                out.aur.push(tail.to_string());
                out.items.push(RequestedItem {
                    name: tail.to_string(),
                    manager: "aur".to_string(),
                    repo: Some("aur".to_string()),
                    installed_version: installed_version(tail),
                    candidate_version: item.version.clone(),
                    status: requested_status(installed_version(tail).as_deref(), &item.version),
                });
            } else if manager.repos().iter().any(|repo| repo == head) {
                let db = manager
                    .handle()
                    .syncdbs()
                    .into_iter()
                    .find(|db| db.name() == head)
                    .ok_or_else(|| {
                        format!("Repository '[{head}]' is configured but has no loaded database; refresh with 'pin -Sy'.")
                    })?;
                let pkg = db.pkg(tail).map_err(|_| {
                    format!("Package '{tail}' was not found in repository '[{head}]'.")
                })?;
                if held_by_name(tail) {
                    return Err(format!("'{tail}' is currently held back and cannot be pulled into this upgrade."));
                }
                if ignored.contains(tail) {
                    return Err(format!("'{tail}' is explicitly ignored; remove the matching --ignore/--ignoregroup entry to include it in this upgrade."));
                }
                if ResolverEngine::is_excluded(tail, head, &config.exclude) {
                    return Err(format!("'{tail}' is excluded from [{head}] by configuration; remove the exclusion to include it in this upgrade."));
                }
                check_requested_delay(tail, pkg.build_date(), config, now_epoch)?;
                let installed = installed_version(tail);
                let candidate = pkg.version().to_string();
                out.native.push(target.clone());
                out.items.push(RequestedItem {
                    name: tail.to_string(),
                    manager: "repo".to_string(),
                    repo: Some(head.to_string()),
                    installed_version: installed.clone(),
                    candidate_version: candidate.clone(),
                    status: requested_status(installed.as_deref(), &candidate),
                });
            } else {
                return Err(format!(
                    "Repository '[{head}]' is not recognized. Configured repositories: {}",
                    manager.repos().join(", ")
                ));
            }
            continue;
        }

        if !crate::utils::is_safe_install_target(target) {
            return Err(format!("Refusing unsafe package target '{target}'."));
        }
        if let Some(alias) = crate::sources::owner(config, target) {
            return Err(format!(
                "'{target}' is owned by upstream source '{alias}'; it is already tracked by 'pin -Syu'. To force a reinstall, run 'pin -S {alias}/{target}'."
            ));
        }
        if ignored.contains(target) {
            return Err(format!("'{target}' is explicitly ignored; remove the matching --ignore/--ignoregroup entry to include it in this upgrade."));
        }
        // Repository pins are policy: a bare requested target resolves from
        // its pinned repository, exactly like a pinned update would. Without
        // a pin, plain repository priority applies.
        let pinned = ResolverEngine::match_pinned_package(target, &pins, &glob_pins);
        if let Some(pinned_repo) = pinned.as_deref() {
            if pinned_repo != "aur" {
                let db = manager
                    .handle()
                    .syncdbs()
                    .into_iter()
                    .find(|db| db.name() == pinned_repo)
                    .ok_or_else(|| {
                        format!("'{target}' is pinned to '[{pinned_repo}]', which is not a registered repository.")
                    })?;
                let pkg = db.pkg(target.as_str()).map_err(|_| {
                    format!("'{target}' is pinned to '[{pinned_repo}]', which does not carry it; adjust the pin or qualify another repository explicitly.")
                })?;
                if held_by_name(target) {
                    return Err(format!("'{target}' is currently held back and cannot be pulled into this upgrade."));
                }
                if ResolverEngine::is_excluded(target, pinned_repo, &config.exclude) {
                    return Err(format!("'{target}' is excluded from [{pinned_repo}] by configuration; remove the exclusion to include it in this upgrade."));
                }
                check_requested_delay(target, pkg.build_date(), config, now_epoch)?;
                let installed = installed_version(target);
                let candidate = pkg.version().to_string();
                out.native.push(format!("{pinned_repo}/{target}"));
                out.items.push(RequestedItem {
                    name: target.clone(),
                    manager: "repo".to_string(),
                    repo: Some(pinned_repo.to_string()),
                    installed_version: installed.clone(),
                    candidate_version: candidate.clone(),
                    status: requested_status(installed.as_deref(), &candidate),
                });
                continue;
            }
            // Pinned to the AUR: skip the sync databases and use the AUR
            // lookup below, mirroring resolver policy for pinned packages.
        }
        // A pin to the AUR skips the sync databases (resolver policy);
        // otherwise fall back to the AUR only when no sync package matches.
        let mut handled_native = false;
        if pinned.as_deref() != Some("aur") {
            if let Some(sync_pkg) = manager
            .handle()
            .syncdbs()
            .into_iter()
            .find_map(|db| db.pkg(target.as_str()).ok().map(|pkg| (db.name().to_string(), pkg.version().to_string(), pkg.build_date(), pkg.isize().max(0))))
        {
            let (repo, candidate, builddate, _isize) = sync_pkg;
            if held_by_name(target) {
                return Err(format!("'{target}' is currently held back and cannot be pulled into this upgrade."));
            }
            if ResolverEngine::is_excluded(target, &repo, &config.exclude) {
                return Err(format!("'{target}' is excluded from [{repo}] by configuration; remove the exclusion to include it in this upgrade."));
            }
            check_requested_delay(target, builddate, config, now_epoch)?;
            let installed = installed_version(target);
            // Qualify with the resolving repository so the executed command
            // installs exactly what the preview approved.
            out.native.push(format!("{repo}/{target}"));
            out.items.push(RequestedItem {
                name: target.clone(),
                manager: "repo".to_string(),
                repo: Some(repo),
                installed_version: installed.clone(),
                candidate_version: candidate.clone(),
                status: requested_status(installed.as_deref(), &candidate),
            });
            handled_native = true;
        }
        }
        if !handled_native {
            let found = aur::query_aur(std::slice::from_ref(target));
            let item = found.get(target).ok_or_else(|| {
                format!("'{target}' was not found in the sync databases or the AUR.")
            })?;
            if held_by_name(target) {
                return Err(format!("'{target}' is currently held back and cannot be pulled into this upgrade."));
            }
            out.aur.push(target.clone());
            out.items.push(RequestedItem {
                name: target.clone(),
                manager: "aur".to_string(),
                repo: Some("aur".to_string()),
                installed_version: installed_version(target),
                candidate_version: item.version.clone(),
                status: requested_status(installed_version(target).as_deref(), &item.version),
            });
        }
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noconfirm_only_autoremoves_pure_orphans_when_requested() {
        let make_orphan = |name: &str, optional_for: Vec<String>| OrphanPackage {
            name: name.into(), version: "1".into(), isize: 1,
            desc: String::new(), is_projected: false, dropped_by: Vec::new(), optional_for,
        };
        let orphans = vec![
            make_orphan("pure", Vec::new()),
            make_orphan("plugin", vec!["installed-parent".into()]),
        ];
        assert!(noconfirm_orphan_selection(&orphans, false).is_empty());
        assert_eq!(noconfirm_orphan_selection(&orphans, true), vec!["pure"]);
        let selected = vec!["pure".into(), "plugin".into(), "no-longer-orphan".into()];
        assert_eq!(revalidate_orphan_selection(&selected, &orphans, false), vec!["pure", "plugin"]);
        assert_eq!(revalidate_orphan_selection(&selected, &orphans, true), vec!["pure"]);
    }

    #[test]
    fn upgrade_targets_reject_option_injection() {
        assert!(validate_upgrade_targets(&["extra/git".into()], &["paru-bin".into()]).is_ok());
        assert!(validate_upgrade_targets(&["--config".into()], &[]).is_err());
        assert!(validate_upgrade_targets(&[], &["-evil".into()]).is_err());
    }

    #[test]
    fn repo_prefetch_downloads_without_installing_or_refreshing() {
        let targets = vec!["core/linux".into(), "extra/git".into()];
        let command = repo_prefetch_command(&targets, &["--ignore", "linux-firmware"]);
        let args: Vec<_> = command.get_args().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(args, [
            "-n", "pacman", "-Sw", "--needed", "--noconfirm",
            "--ignore", "linux-firmware", "--", "core/linux", "extra/git",
        ]);
    }

    #[test]
    fn requested_status_compares_versions() {
        assert_eq!(requested_status(None, "1.0-1"), RequestedStatus::NewInstall);
        assert_eq!(
            requested_status(Some("1.0-1"), "2.0-1"),
            RequestedStatus::Upgrade
        );
        assert_eq!(
            requested_status(Some("2.0-1"), "1.0-1"),
            RequestedStatus::Downgrade
        );
        assert_eq!(
            requested_status(Some("1.0-1"), "1.0-1"),
            RequestedStatus::AlreadyCurrent
        );
    }

    #[test]
    fn ignored_names_collects_comma_lists() {
        let flags = vec![
            "--ignore=a,b".to_string(),
            "--ignore".to_string(),
            "c".to_string(),
            "--noconfirm".to_string(),
        ];
        let got = ignored_names(&flags);
        assert!(got.contains("a") && got.contains("b") && got.contains("c"));
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn collect_ignored_names_expands_groups() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        let mut member = FixturePackage::new("member", "1-1");
        member.groups = &["mygroup"];
        let sync = [member, FixturePackage::new("other", "1-1")];
        let fixture = AlpmFixture::new(&[], &[("extra", &sync)]);
        let flags = vec![
            "--ignore=solo".to_string(),
            "--ignoregroup".to_string(),
            "mygroup".to_string(),
            "--ignoregroup=bogusgroup".to_string(),
        ];
        let got = collect_ignored_names(&fixture.manager, &flags);
        assert!(got.contains("solo"));
        assert!(got.contains("member"));
        assert!(!got.contains("other"));
        assert!(!got.contains(""));
    }

    #[test]
    fn ignored_names_matches_exact_entries() {
        let flags = vec![
            "--ignore=linux,linux-firmware".to_string(),
            "--ignore".to_string(),
            "foo".to_string(),
        ];
        let got = ignored_names(&flags);
        assert!(got.contains("linux"));
        assert!(got.contains("linux-firmware"));
        assert!(got.contains("foo"));
        assert!(!got.contains("bar"));
        assert!(!got.contains("linux-firm"));
    }

    #[test]
    fn requested_targets_classify_native_and_fail_closed() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        use std::collections::HashMap;

        let local = [FixturePackage::new("old", "1.0-1")];
        let sync = [
            FixturePackage::new("newpkg", "1.0-1"),
            FixturePackage::new("old", "2.0-1"),
        ];
        let fixture = AlpmFixture::new(&local[..], &[("extra", &sync[..])]);
        let config = Config::default();
        let res = ResolveResult {
            installed_count: 0,
            repos: Vec::new(),
            packages: HashMap::new(),
            updates: Vec::new(),
            held_packages: Vec::new(),
            unresolved_pins: Vec::new(),
        };

        let out = resolve_requested_targets(
            &config,
            &fixture.manager,
            &res,
            &["newpkg".to_string(), "old".to_string()],
            &HashSet::new(),
        )
        .expect("native requested targets resolve");
        assert_eq!(
            out.native,
            vec!["extra/newpkg".to_string(), "extra/old".to_string()]
        );
        assert!(out.aur.is_empty());
        let statuses: Vec<(String, RequestedStatus)> = out
            .items
            .iter()
            .map(|item| (item.name.clone(), item.status.clone()))
            .collect();
        assert!(statuses.contains(&("newpkg".to_string(), RequestedStatus::NewInstall)));
        assert!(statuses.contains(&("old".to_string(), RequestedStatus::Upgrade)));

        // Repeated targets are shown and executed once.
        let out = resolve_requested_targets(
            &config,
            &fixture.manager,
            &res,
            &["newpkg".to_string(), "extra/newpkg".to_string()],
            &HashSet::new(),
        )
        .expect("repeated targets resolve");
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.native.len(), 1);

        // Unknown repos, unsafe targets, ignored packages, and upstream
        // source targets fail closed without network access.
        assert!(resolve_requested_targets(
            &config,
            &fixture.manager,
            &res,
            &["extra/nope".to_string()],
            &HashSet::new(),
        )
        .is_err());
        assert!(resolve_requested_targets(
            &config,
            &fixture.manager,
            &res,
            &["bogus/pkg".to_string()],
            &HashSet::new(),
        )
        .is_err());
        assert!(resolve_requested_targets(
            &config,
            &fixture.manager,
            &res,
            &["--config".to_string()],
            &HashSet::new(),
        )
        .is_err());
        let mut ignore_old = HashSet::new();
        ignore_old.insert("old".to_string());
        assert!(resolve_requested_targets(
            &config,
            &fixture.manager,
            &res,
            &["old".to_string()],
            &ignore_old,
        )
        .is_err());
        assert!(resolve_requested_targets(
            &config,
            &fixture.manager,
            &res,
            &["github:owner/repo".to_string()],
            &HashSet::new(),
        )
        .is_err());

        let mut alias_config = config.clone();
        alias_config.sources.insert(
            "myalias".to_string(),
            crate::sources::Source {
                kind: crate::sources::Kind::Github,
                url: String::new(),
                token_env: None,
                packages: Default::default(),
            },
        );
        assert!(resolve_requested_targets(
            &alias_config,
            &fixture.manager,
            &res,
            &["myalias/pkg".to_string()],
            &HashSet::new(),
        )
        .is_err());
    }

    fn empty_resolve_result() -> ResolveResult {
        ResolveResult {
            installed_count: 0,
            repos: Vec::new(),
            packages: std::collections::HashMap::new(),
            updates: Vec::new(),
            held_packages: Vec::new(),
            unresolved_pins: Vec::new(),
        }
    }

    #[test]
    fn requested_targets_honor_repo_pins() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};

        let local = [FixturePackage::new("dup", "1.0-1")];
        let repo_a = [FixturePackage::new("dup", "2.0-1")];
        let repo_b = [FixturePackage::new("dup", "1.0-1")];
        let fixture =
            AlpmFixture::new(&local[..], &[("repo-a", &repo_a[..]), ("repo-b", &repo_b[..])]);
        let res = empty_resolve_result();

        // Without a pin, plain repository priority (first sync db) wins.
        let out = resolve_requested_targets(
            &Config::default(),
            &fixture.manager,
            &res,
            &["dup".to_string()],
            &HashSet::new(),
        )
        .expect("unpinned request resolves");
        assert_eq!(out.native, vec!["repo-a/dup".to_string()]);
        assert_eq!(out.items[0].status, RequestedStatus::Upgrade);

        // With a pin, the pinned repository wins even when it is not first.
        let mut pinned_config = Config::default();
        pinned_config.pins.insert("dup".to_string(), "repo-b".to_string());
        let out = resolve_requested_targets(
            &pinned_config,
            &fixture.manager,
            &res,
            &["dup".to_string()],
            &HashSet::new(),
        )
        .expect("pinned request resolves");
        assert_eq!(out.native, vec!["repo-b/dup".to_string()]);
        assert_eq!(out.items[0].status, RequestedStatus::AlreadyCurrent);

        // Glob pins apply too.
        pinned_config.pins.clear();
        pinned_config
            .pins
            .insert("du*".to_string(), "repo-b".to_string());
        let out = resolve_requested_targets(
            &pinned_config,
            &fixture.manager,
            &res,
            &["dup".to_string()],
            &HashSet::new(),
        )
        .expect("glob-pinned request resolves");
        assert_eq!(out.native, vec!["repo-b/dup".to_string()]);

        // A pin to a repository that does not carry the package fails
        // closed instead of silently installing from elsewhere.
        pinned_config.pins.clear();
        pinned_config
            .pins
            .insert("dup".to_string(), "repo-b".to_string());
        let mut missing_config = Config::default();
        missing_config
            .pins
            .insert("dup".to_string(), "repo-missing".to_string());
        assert!(resolve_requested_targets(
            &missing_config,
            &fixture.manager,
            &res,
            &["dup".to_string()],
            &HashSet::new(),
        )
        .is_err());

        // Disabled pinning restores plain priority.
        pinned_config.features.pinning = false;
        let out = resolve_requested_targets(
            &pinned_config,
            &fixture.manager,
            &res,
            &["dup".to_string()],
            &HashSet::new(),
        )
        .expect("unpinned request resolves");
        assert_eq!(out.native, vec!["repo-a/dup".to_string()]);
    }
}
