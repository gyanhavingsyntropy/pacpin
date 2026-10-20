//! Bounded source-package checks with independent, lossless outcomes.
use crate::discovery::{Manager, Phase, Report};
use std::sync::{atomic::{AtomicUsize, Ordering}, Mutex};

const CONCURRENT_CHECKS: usize = 4;

/// Fill a free worker immediately rather than waiting for a whole batch. Keep
/// stable target ordering for previews, regardless of network completion order.
pub(super) fn check_targets<T: Send>(
    targets: &[(String, String)],
    check: impl Fn(&str, &str) -> Result<Option<T>, String> + Sync,
) -> Report<T> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(targets.len()));
    std::thread::scope(|scope| {
        for _ in 0..targets.len().min(CONCURRENT_CHECKS) {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some((target, installed)) = targets.get(index) else { break; };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(target, installed)))
                    .unwrap_or_else(|_| Err("Source check worker failed".into()));
                results.lock().unwrap_or_else(|e| e.into_inner()).push((index, result));
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.sort_by_key(|(index, _)| *index);
    let mut report = Report::default();
    for (index, result) in results {
        report.record(Manager::Source(targets[index].0.clone()), Phase::Updates,
            result.map(|value| value.into_iter().collect()));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn checks_overlap_and_a_slow_first_target_does_not_block_the_next_wave() {
        let targets: Vec<_> = (0..9).map(|i| (format!("repo/pkg{i}"), "1".into())).collect();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let started = AtomicUsize::new(0);
        let (updates, failures) = check_targets(&targets, |target, installed| {
            assert_eq!(installed, "1");
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(count, Ordering::SeqCst);
            started.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(3);
            // A chunked implementation deadlocks here until the timeout.
            if target == "repo/pkg0" {
                while started.load(Ordering::SeqCst) < targets.len() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(started.load(Ordering::SeqCst), targets.len());
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(Some(target.to_string()))
        }).into_parts();
        assert!(failures.is_empty());
        assert_eq!(updates, targets.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>());
        assert!(peak.load(Ordering::SeqCst) >= 2);
        assert!(peak.load(Ordering::SeqCst) <= CONCURRENT_CHECKS);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failure_or_panic_does_not_discard_other_packages_in_the_same_source() {
        let targets: Vec<_> = ["repo/broken", "repo/current", "repo/good", "other/panic", "other/good"]
            .map(|name| (name.into(), "1".into())).into();
        let (updates, failures) = check_targets(&targets, |target, _| match target {
            "repo/broken" => Err("missing asset".into()),
            "repo/current" => Ok(None),
            "other/panic" => panic!("injected metadata panic"),
            _ => Ok(Some(target.to_string())),
        }).into_parts();
        assert_eq!(updates, ["repo/good", "other/good"]);
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].manager, Manager::Source("repo/broken".into()));
        assert_eq!(failures[1].manager, Manager::Source("other/panic".into()));
        assert_eq!(failures[1].detail, "Source check worker failed");
        assert!(check_targets::<()>(&[], |_, _| unreachable!()).require_complete().unwrap().is_empty());
    }
}
