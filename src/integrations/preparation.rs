//! One owner for optional, approved download preparation across manager stages.
//! Preparation never establishes installed success or changes transaction order.
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use super::{ExternalUpdate, IntegrationProvider};
use crate::progress::{self, Manager, transfers::{self, Window}};
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Policy {
    /// Private verified release artifacts; may overlap a later CPU phase.
    Background,
    /// Backend cache writes have no reserved cross-manager space budget. Prepare
    /// immediately before its own stage, after earlier writers have completed.
    BeforeStage,
    Unsupported(&'static str),
}

struct Job {
    provider: Arc<dyn IntegrationProvider>,
    updates: Vec<ExternalUpdate>,
    manager: Manager,
    order: usize,
    policy: Policy,
    state: Mutex<State>,
}
#[derive(Default)]
struct State { started: bool, handle: Option<JoinHandle<Result<(), String>>> }

impl Job {
    fn start(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.started { return; }
        state.started = true;
        let provider = self.provider.clone();
        let updates = self.updates.clone();
        let manager = self.manager;
        state.handle = Some(std::thread::spawn(move || {
            let _activity = progress::activity(manager, "Preparing downloads");
            provider.prefetch(&updates).map_err(|error| error.to_string())
        }));
    }
    fn join(&self) {
        let handle = self.state.lock().unwrap_or_else(|e| e.into_inner()).handle.take();
        if let Some(handle) = handle {
            if !handle.is_finished() {
                println!(":: Waiting for {} downloads to finish...", if self.manager == Manager::Sources { "source" } else { self.provider.name() });
                progress::wait_for_preparation(self.manager, &handle);
            }
            match handle.join() {
                Ok(Ok(())) => {},
                Ok(Err(error)) => eprintln!(":: {} preparation failed: {}.", self.provider.name(), crate::utils::sanitize_display_text(&error)),
                Err(_) => eprintln!(":: {} preparation worker failed.", self.provider.name()),
            }
            if self.manager == Manager::Sources { progress::finish_sources(); }
        }
    }
}

fn on_window(jobs: &[Job], closed: &Mutex<bool>, stages: &[Manager], manager: Manager, window: Window) {
    let closed = closed.lock().unwrap_or_else(|e| e.into_inner());
    if *closed || window != Window::Computing { return; }
    let Some(order) = stages.iter().position(|m| *m == manager) else { return; };
    for job in jobs.iter().filter(|job| job.order > order && job.policy == Policy::Background) {
        job.start();
    }
}

pub(crate) struct Session {
    jobs: Arc<Vec<Job>>,
    closed: Arc<Mutex<bool>>,
    _subscription: transfers::Subscription,
}

impl Session {
    pub fn new(providers: &[Arc<dyn IntegrationProvider>], updates: &[ExternalUpdate], stages: Vec<Manager>) -> Self {
        let jobs: Vec<_> = providers.iter().filter_map(|provider| {
            let updates: Vec<_> = updates.iter().filter(|u| u.runner == provider.name()).cloned().collect();
            if updates.is_empty() { return None; }
            let manager = progress::integration_manager(provider.name());
            let order = stages.iter().position(|m| *m == manager)?;
            Some(Job { provider: provider.clone(), updates, manager, order,
                policy: provider.preparation_policy(), state: Mutex::new(State::default()) })
        }).collect();
        let jobs = Arc::new(jobs);
        let subscribed = jobs.clone();
        let closed = Arc::new(Mutex::new(false));
        let callback_closed = closed.clone();
        let subscription = transfers::subscribe(Arc::new(move |manager, window| {
            on_window(&subscribed, &callback_closed, &stages, manager, window);
        }));
        Self { jobs, closed, _subscription: subscription }
    }

    pub fn before(&self, name: &str) {
        let closed = self.closed.lock().unwrap_or_else(|e| e.into_inner());
        if *closed { return; }
        if let Some(job) = self.jobs.iter().find(|job| job.provider.name() == name) {
            if !matches!(job.policy, Policy::Unsupported(_)) { job.start(); drop(closed); job.join(); }
        }
    }

    /// Failed earlier transactions join only work already started. Never start
    /// a waiting job while finalizing an aborted plan.
    pub fn finish(&self) {
        *self.closed.lock().unwrap_or_else(|e| e.into_inner()) = true;
        for job in self.jobs.iter() { job.join(); }
        progress::clear_sources();
    }
}
impl Drop for Session { fn drop(&mut self) { self.finish(); } }

impl Policy {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Background => "eligible release downloads may overlap earlier verification/build/install phases; otherwise prepare at this stage",
            Self::BeforeStage => "download-only preparation immediately before this manager's upgrade",
            Self::Unsupported(reason) => reason,
        }
    }
}
