//! Concurrent discovery and approved stages, independent of terminal ownership.
//! Guards remove only their own rows; checking never establishes upgrade success.
use super::Manager;
use std::sync::Mutex;

#[derive(Clone)]
enum Phase {
    Pending,
    NotRun,
    Activity(&'static str),
}

#[derive(Default)]
struct Registry {
    next_id: u64,
    rows: Vec<(u64, Manager, Phase)>,
}

impl Registry {
    fn insert(&mut self, manager: Manager, phase: Phase) -> u64 {
        self.next_id += 1;
        self.rows.push((self.next_id, manager, phase));
        self.next_id
    }

    fn remove(&mut self, ids: &[u64]) {
        self.rows.retain(|(id, _, _)| !ids.contains(id));
    }

    fn finished(&mut self, manager: Manager) {
        self.rows.retain(|(_, m, phase)| *m != manager || matches!(phase, Phase::Activity(_)));
    }

    fn not_run(&mut self) {
        for (_, manager, phase) in &mut self.rows {
            // Successful managers may still clean their caches after another
            // stage fails; finalization owns that decision.
            if matches!(phase, Phase::Pending) && *manager != Manager::Cache { *phase = Phase::NotRun; }
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut snapshot = Snapshot::default();
        for (_, manager, phase) in &self.rows {
            let target = match phase {
                Phase::Activity(_) => &mut snapshot.activities,
                _ => &mut snapshot.pending,
            };
            if target.iter().any(|(m, _)| m == manager) { continue; }
            let text = match phase {
                Phase::Pending => "Pending",
                Phase::NotRun => "Not run (earlier failure)",
                Phase::Activity(label) => label,
            };
            target.push((*manager, text.to_string()));
        }
        snapshot
    }
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry { next_id: 0, rows: Vec::new() });

/// Scope owns its row IDs, so a completed worker cannot clear another worker.
pub struct Scope(Vec<u64>);
impl Drop for Scope {
    fn drop(&mut self) {
        REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

pub fn pending(managers: &[Manager]) -> Scope {
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let mut unique = Vec::new();
    let mut ids = Vec::new();
    for &manager in managers {
        if !unique.contains(&manager) {
            unique.push(manager);
            ids.push(registry.insert(manager, Phase::Pending));
        }
    }
    Scope(ids)
}

pub fn activity(manager: Manager, label: &'static str) -> Scope {
    Scope(vec![REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).insert(manager, Phase::Activity(label))])
}

pub(super) fn finished(manager: Manager) {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).finished(manager);
}

pub fn stop_pending() {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).not_run();
}

pub(super) fn clear() {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).rows.clear();
}

#[derive(Default)]
pub(super) struct Snapshot {
    pub activities: Vec<(Manager, String)>,
    pub(super) pending: Vec<(Manager, String)>,
}

impl Snapshot {
    pub fn append(&self, active: &mut Vec<String>, visible: &mut Vec<Manager>) {
        // Actual concurrent work has priority over stages waiting to start.
        for (manager, text) in self.activities.iter().chain(&self.pending) {
            if visible.contains(manager) { continue; }
            active.push(format!("{}: {text}", manager.label()));
            visible.push(*manager);
        }
    }
}

pub(super) fn snapshot() -> Snapshot {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_has_separate_rows_and_never_claims_upgrade_completion() {
        let mut registry = Registry::default();
        let flatpak = registry.insert(Manager::Flatpak, Phase::Activity("Checking updates"));
        registry.insert(Manager::Nix, Phase::Activity("Checking updates"));
        let mut rows = Vec::new();
        registry.snapshot().append(&mut rows, &mut Vec::new());
        assert_eq!(rows, ["Flatpak: Checking updates", "Nix: Checking updates"]);
        registry.remove(&[flatpak]);
        assert_eq!(registry.snapshot().activities, [(Manager::Nix, "Checking updates".into())]);
    }

    #[test]
    fn approved_aur_and_integrations_stay_pending_during_native_work() {
        let mut registry = Registry::default();
        for manager in [Manager::Native, Manager::Aur, Manager::Flatpak, Manager::Nix] {
            registry.insert(manager, Phase::Pending);
        }
        let mut rows = vec!["Pacman: Downloading packages (25%)".into()];
        registry.snapshot().append(&mut rows, &mut vec![Manager::Native]);
        assert_eq!(rows, ["Pacman: Downloading packages (25%)", "AUR: Pending", "Flatpak: Pending", "Nix: Pending"]);
        registry.finished(Manager::Native);
        registry.finished(Manager::Aur);
        assert_eq!(registry.snapshot().pending.len(), 2);
    }

    #[test]
    fn source_download_overrides_pending_row_without_duplicate_manager() {
        let mut registry = Registry::default();
        registry.insert(Manager::Sources, Phase::Pending);
        registry.insert(Manager::Flatpak, Phase::Pending);
        let mut rows = vec!["Sources: Downloading (50%)".into()];
        registry.snapshot().append(&mut rows, &mut vec![Manager::Sources]);
        assert_eq!(rows, ["Sources: Downloading (50%)", "Flatpak: Pending"]);
    }

    #[test]
    fn failure_blocks_waiting_stages_and_scope_cleanup_keeps_other_rows() {
        let mut registry = Registry::default();
        let aur = registry.insert(Manager::Aur, Phase::Pending);
        registry.insert(Manager::Nix, Phase::Pending);
        registry.insert(Manager::Cache, Phase::Pending);
        registry.not_run();
        registry.remove(&[aur]);
        assert_eq!(registry.snapshot().pending, [(Manager::Nix, "Not run (earlier failure)".into()), (Manager::Cache, "Pending".into())]);
    }
}
