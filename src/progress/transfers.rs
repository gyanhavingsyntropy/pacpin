//! Interpret transfer output once for both footer metrics and preparation gates.
//! Percentages describe only their stated scope; they never establish success.
use super::{Manager, observer::{clean_text, downloads_finished}};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Window { Started, Downloading, Computing }
type Listener = Arc<dyn Fn(Manager, Window) + Send + Sync>;
static LISTENER: Mutex<Option<Listener>> = Mutex::new(None);

pub(crate) struct Subscription(Option<Listener>);
impl Drop for Subscription {
    fn drop(&mut self) { *LISTENER.lock().unwrap_or_else(|e| e.into_inner()) = self.0.take(); }
}
pub(crate) fn subscribe(listener: Listener) -> Subscription {
    Subscription(LISTENER.lock().unwrap_or_else(|e| e.into_inner()).replace(listener))
}
pub(super) fn emit(manager: Manager, event: Window) {
    let listener = LISTENER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(listener) = listener { listener(manager, event); }
}

#[derive(Debug, PartialEq, Eq)]
struct Transfer { label: &'static str, percent: Option<u8> }

fn percent(word: &str) -> Option<u8> {
    let value = word.trim_end_matches([',', ')']).strip_suffix('%')?.parse::<f64>().ok()?;
    (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(value as u8)
}

fn curl_time(word: &str) -> bool {
    if matches!(word, "--:--" | "--:--:--") { return true; }
    let parts: Vec<_> = word.split(':').collect();
    matches!(parts.len(), 2 | 3) && parts.iter().all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn transfer(manager: Manager, line: &str) -> Option<Transfer> {
    // An explicit protocol also lets custom wrappers report truthful progress
    // without the renderer guessing from arbitrary diagnostic percentages.
    if let Some(fields) = line.strip_prefix("PACPIN_PROGRESS download ") {
        let (done, total) = fields.trim().split_once('/')?;
        let (done, total) = (done.parse::<u64>().ok()?, total.parse::<u64>().ok()?);
        if total == 0 || done > total { return None; }
        return Some(Transfer { label: "Downloading (operation)", percent: Some((u128::from(done)*100/u128::from(total)) as u8) });
    }
    if manager == Manager::Flatpak {
        let percent = line.split_whitespace().find_map(percent)?;
        if line.starts_with("Downloading") { return Some(Transfer { label: "Downloading (ref)", percent: Some(percent) }); }
        if line.starts_with("Updating") || line.starts_with("Installing") { return Some(Transfer { label: "Updating (ref)", percent: Some(percent) }); }
        return None;
    }
    if manager != Manager::Aur { return None; }
    // ALPM also uses hash bars for install/verification counters and an
    // aggregate Total row. Those belong to the native stage arithmetic.
    if line.starts_with('(') || line.starts_with("Total ") || line.starts_with("Total(") { return None; }
    let words: Vec<_> = line.split_whitespace().collect();
    // curl's normal meter has no percent sign: %Total Total %Received Received
    // %Xferd Xferd Speed Speed Time Time Time Current. Received is the byte
    // fraction; Total can mix upload/download and must not replace it.
    if words.len() == 12 && [0, 2, 4].iter().all(|&i| words[i].parse::<u8>().is_ok_and(|p| p <= 100))
        && words[8..11].iter().all(|word| curl_time(word)) {
        return Some(Transfer { label: "Downloading (file)", percent: words[2].parse().ok() });
    }
    if line.starts_with("Receiving objects:") || line.starts_with("remote: Counting objects:") {
        return Some(Transfer { label: "Fetching Git objects", percent: words.iter().find_map(|w| percent(w)) });
    }
    // curl --progress-bar and wget show a percent sign. Require their actual
    // bar/rate/size shape; a compiler saying "80% complete" isn't a transfer.
    let value = words.iter().find_map(|w| percent(w));
    if value.is_some() && (line.contains("###") || words.iter().any(|w| w.ends_with("/s"))) {
        return Some(Transfer { label: "Downloading (file)", percent: value });
    }
    None
}

pub(super) fn footer(manager: Manager, line: &str) -> Option<String> {
    let line = clean_text(line);
    let transfer = transfer(manager, line.trim())?;
    Some(match transfer.percent {
        Some(percent) => format!("{} ({percent}%)", transfer.label),
        None => transfer.label.into(),
    })
}

pub(super) fn window(manager: Manager, line: &str) -> Option<Window> {
    let line = clean_text(line);
    let line = line.trim();
    if transfer(manager, line).is_some() { return Some(Window::Downloading); }
    if line == "PACPIN_PROGRESS phase=build" || line == "PACPIN_PROGRESS phase=install" || line == "PACPIN_PROGRESS phase=verify" {
        return Some(Window::Computing);
    }
    if matches!(manager, Manager::Native | Manager::Sources) && downloads_finished(line) {
        return Some(Window::Computing);
    }
    if matches!(manager, Manager::Aur | Manager::Sources) && [
        "==> Starting build()", "==> Starting check()", "==> Starting package()",
        "==> Entering fakeroot environment", "==> Creating package", "==> Compressing package",
    ].iter().any(|marker| line.starts_with(marker)) { return Some(Window::Computing); }
    if matches!(manager, Manager::Aur | Manager::Sources) && [
        "==> Retrieving sources", "==> Downloading", "Cloning into ",
    ].iter().any(|marker| line.starts_with(marker)) { return Some(Window::Downloading); }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn curl_and_git_percentages_have_explicit_transfer_scope() {
        assert_eq!(footer(Manager::Aur, " 40 100M 25 25M 0 0 1200k 0 0:01:00 0:00:15 0:00:45 1200k"), Some("Downloading (file) (25%)".into()));
        assert_eq!(footer(Manager::Aur, "29 0.98M 29 301.2k 0 0 34498 0 00:29 00:08 00:21 90596"), Some("Downloading (file) (29%)".into()));
        assert_eq!(footer(Manager::Aur, "0 100M 0 0 0 0 0 0 --:-- --:-- --:-- 0"), Some("Downloading (file) (0%)".into()));
        assert_eq!(footer(Manager::Aur, "Receiving objects:  63% (63/100), 2 MiB | 1 MiB/s"), Some("Fetching Git objects (63%)".into()));
        assert_eq!(footer(Manager::Aur, "############ 72.4%"), Some("Downloading (file) (72%)".into()));
        assert_eq!(footer(Manager::Aur, "[######## ] 72%"), Some("Downloading (file) (72%)".into()));
        assert_eq!(footer(Manager::Aur, "Compiling crate 80% complete"), None);
        assert_eq!(footer(Manager::Native, "[######## ] 72%"), None);
        assert_eq!(footer(Manager::Aur, "(1/2) upgrading package [####] 100%"), None);
        assert_eq!(footer(Manager::Aur, "Total (2/2) 1 MiB [####] 100%"), None);
    }
    #[test]
    fn readiness_uses_manager_phases_and_not_a_finished_individual_file() {
        assert_eq!(window(Manager::Aur, "Receiving objects: 100% (1/1)"), Some(Window::Downloading));
        assert_eq!(window(Manager::Aur, "==> Starting build()..."), Some(Window::Computing));
        assert_eq!(window(Manager::Aur, "checking keys in keyring"), None); // may be a build dependency
        assert_eq!(window(Manager::Native, "checking keys in keyring"), Some(Window::Computing));
        assert_eq!(window(Manager::Flatpak, "Installing..."), None); // backend may still fetch
        assert_eq!(window(Manager::Nix, "PACPIN_PROGRESS phase=build"), Some(Window::Computing));
        assert_eq!(footer(Manager::Nix, "PACPIN_PROGRESS download 32/80"), Some("Downloading (operation) (40%)".into()));
        assert_eq!(footer(Manager::Pipx, "PACPIN_PROGRESS download 8/0"), None);
    }
}
