//! Progress estimates and concise labels; never decide transaction success.
use super::{Manager, observer::clean_text};

#[derive(Default)]
pub(super) struct NativeDownloads {
    pub(super) packages: Vec<(String, u64, u8)>,
    pub(super) ambiguous: bool,
    pub(super) aggregate: Option<u8>,
    pub(super) observed: bool,
}
impl NativeDownloads {
    pub(super) fn new(targets: &[(String, u64)]) -> Self {
        Self { packages: targets.iter().map(|(name, size)| (name.clone(), *size, 0)).collect(), ..Default::default() }
    }
    pub(super) fn observe(&mut self, line: &str) {
        let Some(percent) = line_percent(line) else { return; };
        let Some(label) = line.split_whitespace().next() else { return; };
        if label == "Total" || label.starts_with("Total(") { self.aggregate = Some(percent); return; }
        if label.starts_with('(') { return; }
        let matching: Vec<_> = self.packages.iter_mut().filter(|(name, _, _)| {
            label == name.as_str() || label.strip_suffix("...").is_some_and(|prefix| !prefix.is_empty() && name.starts_with(prefix))
        }).collect();
        if matching.len() == 1 {
            matching.into_iter().next().unwrap().2 = percent;
            self.observed = true;
        } else { self.ambiguous = true; }
    }
    pub(super) fn phase(&mut self, manager: Manager, line: &str, previous: &str) -> String {
        self.observe(line);
        let phase = phase_text(manager, line, previous);
        if manager != Manager::Native || !phase.starts_with("Downloading packages") || line.trim_start().starts_with("Total") { return phase; }
        if let Some(percent) = self.aggregate { return format!("Downloading packages ({percent}%)"); }
        let total: u128 = self.packages.iter().map(|(_, size, _)| u128::from(*size)).sum();
        if total == 0 || self.ambiguous || !self.observed { return phase; }
        let percent = self.packages.iter().map(|(_, size, progress)| u128::from(*size) * u128::from(*progress)).sum::<u128>() / total;
        format!("Downloading packages ({percent}%)")
    }
}

fn line_percent(text: &str) -> Option<u8> {
    text.split_whitespace().find_map(|word| word.strip_suffix('%').and_then(|number| number.parse::<u8>().ok()).filter(|&value| value <= 100))
}


pub(super) fn download_text(name: &str, downloaded: u64, total: Option<u64>) -> String {
    let mib = downloaded as f64 / 1_048_576.0;
    match total.filter(|&n| n != 0) {
        Some(total) => {
            let percent = (downloaded.saturating_mul(100) / total).min(100);
            let filled = (percent / 10) as usize;
            format!(
                "{name} [{}{}] {mib:.1}/{:.1} MiB ({percent}%)",
                "=".repeat(filled),
                " ".repeat(10 - filled),
                total as f64 / 1_048_576.0
            )
        }
        None => format!("{name} [{mib:.1} MiB]"),
    }
}

pub(super) fn footer_download(name: &str, downloaded: u64, total: Option<u64>, width: usize) -> String {
    let mut suffix = download_text("", downloaded, total);
    if crate::ui::str_width(&suffix) > width {
        if let Some(total) = total.filter(|&n| n != 0) {
            let percent = (downloaded.saturating_mul(100) / total).min(100);
            suffix = format!(
                " {:.0}/{:.0}MiB {percent}%",
                downloaded as f64 / 1_048_576.0,
                total as f64 / 1_048_576.0
            );
        }
    }
    let name_width = width.saturating_sub(crate::ui::str_width(&suffix));
    format!(
        "{}{}",
        crate::ui::truncate_str(&clean_text(name), name_width),
        suffix
    )
}

pub(super) fn native_footer(text: &str, width: usize) -> String {
    let clean = clean_text(text);
    if let Some((phase, metrics)) = clean.rsplit_once(" (").filter(|(_, metrics)| metrics.ends_with(')') && metrics.contains('%')) {
        let suffix = format!(" ({metrics}");
        if crate::ui::str_width(&suffix) + 2 <= width {
            return format!("{}{suffix}", crate::ui::truncate_str(phase, width - crate::ui::str_width(&suffix)));
        }
        if let Some(percent) = metrics.split(',').next().filter(|percent| percent.trim_end_matches(')').ends_with('%')) {
            let suffix = format!(" ({})", percent.trim_end_matches(')'));
            return format!("{}{suffix}", crate::ui::truncate_str(phase, width.saturating_sub(crate::ui::str_width(&suffix))));
        }
    }
    let words: Vec<_> = clean
        .split_whitespace()
        .filter(|word| {
            // Pacman's long ASCII bar is already visible in the normal text area.
            !(word.starts_with('[')
                && word.ends_with(']')
                && word[1..word.len() - 1]
                    .chars()
                    .all(|c| matches!(c, '#' | '-' | '=' | '>')))
        })
        .collect();
    if let Some(unit) = words
        .iter()
        .position(|word| matches!(*word, "B" | "KiB" | "MiB" | "GiB"))
        .filter(|&unit| unit > 0)
    {
        let suffix = words[unit - 1..].join(" ");
        let name = words[..unit - 1].join(" ");
        let name_width = width.saturating_sub(crate::ui::str_width(&suffix) + 1);
        return format!("{} {suffix}", crate::ui::truncate_str(&name, name_width));
    }
    crate::ui::truncate_str(&words.join(" "), width)
}

pub(super) fn phase_text(manager: Manager, text: &str, previous: &str) -> String {
    if manager == Manager::Hooks { return previous.into(); }
    if let Some(transfer) = super::transfers::footer(manager, text) { return transfer; }
    let clean = clean_text(text);
    let lower = clean.to_lowercase();
    if matches!(manager, Manager::Aur | Manager::Sources) {
        if lower.starts_with("==> starting build()") || lower.starts_with("==> starting check()") {
            return "Building packages".into();
        }
        if ["==> starting package()", "==> entering fakeroot", "==> creating package", "==> compressing package"].iter().any(|marker| lower.starts_with(marker)) {
            return "Packaging source".into();
        }
        if lower.starts_with("==> finished making:") { return "Build complete; awaiting installation".into(); }
        // Configure/compiler output is not a package-manager lifecycle signal.
        // In particular, "checking ...", optional feature failures and password
        // checks must not replace Building or suspend the footer for sudo.
        if matches!(previous, "Building packages" | "Packaging source") {
            let counter = lower.strip_prefix('(').and_then(|rest| rest.split_once(')'))
                .and_then(|(counter, _)| counter.split_once('/'))
                .is_some_and(|(done, total)| done.trim().parse::<u64>().is_ok() && total.trim().parse::<u64>().is_ok());
            let native = ["checking keys in keyring", "checking keyring", "checking package integrity", "loading package files", "checking for file conflicts", "checking available disk space", "resolving dependencies", "looking for conflicting packages"]
                .iter().any(|marker| lower.starts_with(marker));
            if !clean.starts_with("==>") && !clean.starts_with("::") && !counter && !native && !lower.starts_with("error:") && !lower.starts_with("pacpin_progress ") {
                return previous.into();
            }
        }
    }
    match lower.as_str() {
        "pacpin_progress phase=build" => return "Building packages".into(),
        "pacpin_progress phase=install" => return "Installing packages".into(),
        "pacpin_progress phase=verify" => return "Verifying packages".into(),
        _ => {},
    }
    if manager == Manager::Nix && lower.contains("copying path") { return "Downloading store paths".into(); }
    if lower.contains("waiting for repository downloads") { return "Queued; waiting for repository downloads".into(); }
    if lower.contains("prefetch failed") { return "Prefetch failed; retry pending".into(); }
    if lower.contains("enter a number") || lower.contains("enter a selection") { return "Awaiting package selection".into(); }
    if lower.contains("password") || lower.contains("authentication") { return "Awaiting authorization".into(); }
    if lower.contains("[y/n]") || lower.contains("[y/n") || lower.contains("proceed with") { return "Awaiting confirmation".into(); }
    if lower.starts_with("error:") || lower.starts_with("==> error:") || lower.contains("failed") { return "Operation failed".into(); }
    let stage = |phase: &str| phase_with_metrics(manager, &clean, phase);
    if manager == Manager::Refresh { return stage("Refreshing repositories"); }
    if manager == Manager::Record { return previous.into(); }
    if manager == Manager::Orphans {
        if lower.contains("checking dependencies") { return "Checking dependencies".into(); }
        if lower.contains("removing") || lower.contains("processing package changes") { return stage("Removing packages"); }
        if lower.contains("hooks") { return "Running removal hooks".into(); }
        return previous.into();
    }
    if manager == Manager::Cache {
        if lower.contains("unused repositories") { return "Cleaning repository metadata".into(); }
        if lower.contains("download") && lower.contains("director") { return "Clearing download leftovers".into(); }
        if lower.contains("removing all files") || lower.contains("cleaning") { return "Cleaning package cache".into(); }
        return previous.into();
    }
    if lower.contains("dkms") { return stage("Building kernel modules"); }
    if lower.contains("post-transaction") || lower.contains("hooks") { return stage("Running post-install hooks"); }
    if lower.contains("resolving dependencies") || lower.contains("conflicting packages") { return stage("Resolving dependencies"); }
    if lower.contains("checking") || lower.contains("verifying") { return stage("Verifying packages"); }
    if lower.contains("completed download") || lower.contains("prefetched artifact") { return "Download verified; ready to package".into(); }
    if lower.contains("fetching") || lower.contains("prefetching") || lower.contains("downloading") || lower.contains("retrieving packages") { return stage("Downloading packages"); }
    if lower.contains("creating package") || lower.contains("compressing package") || lower.contains("packaging") { return "Packaging source".into(); }
    if lower.contains("building") || lower.contains("makepkg") || lower.contains("starting build()") || lower.contains("starting check()") || lower.contains("starting package()") { return stage("Building packages"); }
    if lower.contains("upgrading") || lower.contains("installing") || lower.contains("processing package changes") { return stage("Installing packages"); }
    if matches!(manager, Manager::Native | Manager::Aur) && clean.contains('%') {
        return stage(previous.split(" (").next().unwrap_or(previous));
    }
    previous.into()
}

pub(super) fn phase_with_metrics(manager: Manager, text: &str, phase: &str) -> String {
    if !matches!(manager, Manager::Native | Manager::Aur | Manager::Orphans) { return phase.into(); }
    if let Some(counter) = text.trim_start().strip_prefix('(').and_then(|line| line.split_once(')').map(|(counter, _)| counter.trim())) {
        if let Some((done, total)) = counter.split_once('/').and_then(|(done, total)| Some((done.trim().parse::<u64>().ok()?, total.trim().parse::<u64>().ok()?))).filter(|(_, total)| *total != 0) {
            let done = done.min(total);
            let progress = line_percent(text).map(|percent| u128::from(done.saturating_sub(1)) * 100 + u128::from(percent)).unwrap_or(u128::from(done) * 100);
            // ALPM's keyring/integrity/loading/conflict/space bars already
            // describe the full verification action. Package installation
            // and removal bars describe the current package instead.
            let percent = if phase == "Verifying packages" {
                line_percent(text).map(u128::from).unwrap_or(u128::from(done) * 100 / u128::from(total))
            } else { (progress / u128::from(total)).min(100) };
            return format!("{phase} ({percent}%, {done}/{total})");
        }
    }
    // Individual download bars are not a transaction percentage. Prefer
    // pacman's aggregate Total bar; planned downloads are weighted separately.
    if text.trim_start().strip_prefix("Total").is_some_and(|rest| rest.starts_with(char::is_whitespace) || rest.starts_with('(')) {
        if let Some(percent) = line_percent(text) { return format!("{phase} ({percent}%)"); }
    }
    phase.into()
}

pub(super) fn scoped_phase(manager: Manager, label: &str, phase: &str) -> String {
    if manager == Manager::Cache {
        let scope = if label.contains("native") { "Native" } else if label.contains("AUR") { "AUR" }
            else if let Some(scope) = label.strip_prefix("Cleaning ").and_then(|label| label.strip_suffix(" cache")).filter(|scope| !scope.is_empty() && *scope != "package") { scope }
            else { return phase.into(); };
        let phase = if phase == label { "Cleaning package cache" } else { phase };
        return format!("{scope}: {phase}");
    }
    phase.into()
}
