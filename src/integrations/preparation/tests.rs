use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Probe { policy: Policy, calls: Arc<AtomicUsize>, fail: bool }
impl IntegrationProvider for Probe {
    fn name(&self) -> &str { "Nix" }
    fn is_available(&self) -> bool { true }
    fn refresh_metadata(&self) -> std::io::Result<()> { Ok(()) }
    fn check_updates(&self) -> crate::discovery::Report<ExternalUpdate> { crate::discovery::Report::updates("Nix", Ok(Vec::new())) }
    fn upgrade_actions(&self, _: &[ExternalUpdate]) -> std::io::Result<Vec<crate::integrations::UpgradeAction>> { Ok(Vec::new()) }
    fn execute_upgrade(&self, _: &[ExternalUpdate]) -> std::io::Result<bool> { Ok(true) }
    fn check_options(&self, _: crate::scratch::Policy, _: bool) -> Result<(), crate::runtime::OptionError> { Ok(()) }
    fn clean_command_str(&self) -> String { String::new() }
    fn execute_clean(&self) -> std::io::Result<bool> { Ok(true) }
    fn preparation_policy(&self) -> Policy { self.policy }
    fn prefetch(&self, updates: &[ExternalUpdate]) -> std::io::Result<()> {
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].id, "approved");
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail { Err(std::io::Error::other("preparation failed")) } else { Ok(()) }
    }
}
fn job(order: usize, policy: Policy, fail: bool) -> (Job, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(Probe { calls: calls.clone(), policy, fail });
    (Job { provider, updates: vec![ExternalUpdate { runner: "Nix".into(), id: "approved".into(), name: "approved".into(), repo: "fixture".into(),
        version: "2".into(), installed_version: Some("1".into()), scope: None, revision: None }],
        manager: Manager::Nix, order, policy, state: Mutex::new(State::default()) }, calls)
}

#[test]
fn computing_starts_only_later_background_jobs_once() {
    let (background, calls) = job(1, Policy::Background, false);
    let (before, before_calls) = job(2, Policy::BeforeStage, false);
    let (unsupported, unsupported_calls) = job(3, Policy::Unsupported("unsupported"), false);
    let jobs = [background, before, unsupported];
    let closed = Mutex::new(false);
    let stages = [Manager::Aur, Manager::Nix, Manager::Flatpak, Manager::Pipx];
    for event in [Window::Started, Window::Downloading] {
        on_window(&jobs, &closed, &stages, Manager::Aur, event);
        assert!(!jobs[0].state.lock().unwrap().started);
    }
    on_window(&jobs, &closed, &stages, Manager::Pipx, Window::Computing);
    on_window(&jobs, &closed, &stages, Manager::Native, Window::Computing);
    assert!(!jobs[0].state.lock().unwrap().started);
    for _ in 0..3 { on_window(&jobs, &closed, &stages, Manager::Aur, Window::Computing); }
    jobs[0].join();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(before_calls.load(Ordering::SeqCst), 0);
    assert_eq!(unsupported_calls.load(Ordering::SeqCst), 0);
    assert!(!jobs[1].state.lock().unwrap().started);
}

#[test]
fn closed_plan_cannot_start_waiting_work() {
    let (background, calls) = job(1, Policy::Background, false);
    let jobs = [background];
    on_window(&jobs, &Mutex::new(true), &[Manager::Aur, Manager::Nix], Manager::Aur, Window::Computing);
    jobs[0].join();
    assert!(!jobs[0].state.lock().unwrap().started);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn preparation_failure_is_not_installation_failure_or_success() {
    let (job, calls) = job(0, Policy::BeforeStage, true);
    job.start();
    job.join(); // reports failure; does not return successful installed work
    job.start();
    job.join();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(job.provider.execute_upgrade(&job.updates).unwrap()); // normal stage still retries
}
