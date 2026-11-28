//! CLI handling for ephemeral host execution and sandboxed execution.

use std::process::exit;

use colored::Colorize;

use crate::config::Config;
use crate::sandbox::{self, TryOptions};

#[derive(Clone, Copy)]
enum Mode {
    Run,
    Try,
}

fn parse_args(args: &[String], mode: Mode) -> (Option<String>, Vec<String>, TryOptions) {
    let mut options = match mode {
        Mode::Run => TryOptions::for_run(),
        Mode::Try => TryOptions::default(),
    };
    let mut target = None;
    let mut pass_through = Vec::new();
    let mut after_delimiter = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        if after_delimiter {
            pass_through.push(arg.clone());
        } else if arg == "--" {
            after_delimiter = true;
        } else if matches!(mode, Mode::Run) && (arg == "--sandbox" || arg == "--sandboxed") {
            options.no_sandbox = false;
            options.is_run_mode = false;
            options.share_net = false;
            options.rw_cwd = false;
        } else if matches!(mode, Mode::Try)
            && (arg == "--no-sandbox" || arg == "--bare" || arg == "--unsandboxed")
        {
            options.no_sandbox = true;
        } else if matches!(mode, Mode::Try)
            && (arg == "--net" || arg == "--network" || arg == "--share-net")
        {
            options.share_net = true;
        } else if matches!(mode, Mode::Try)
            && (arg == "--rw" || arg == "--rw-cwd" || arg == "--write-cwd")
        {
            options.rw_cwd = true;
        } else if matches!(mode, Mode::Try) && arg == "--gui" {
            options.gui = true;
        } else if matches!(mode, Mode::Try) && arg == "--audio" {
            options.audio = true;
        } else if arg == "--allow-unverified" {
            options.allow_unverified = true;
        } else if arg == "--installed" {
            options.installed = true;
        } else if matches!(arg.as_str(), "--no-cache" | "--clean-cache") {
            options.no_cache = true;
        } else if arg == "--dry-run" {
            options.dry_run = true;
        } else if arg == "--noconfirm" || arg == "-y" {
            options.noconfirm = true;
        } else if arg == "--bin" {
            if let Some(value) = args.get(i + 1) {
                options.bin = Some(value.clone());
                i += 1;
            }
        } else if let Some(value) = arg.strip_prefix("--bin=") {
            options.bin = Some(value.to_string());
        } else if target.is_none() && !arg.starts_with('-') {
            target = Some(arg.clone());
        } else {
            pass_through.push(arg.clone());
        }
        i += 1;
    }

    (target, pass_through, options)
}

fn write_mode_help(mode: Mode, out: &mut impl std::io::Write) {
    match mode {
        Mode::Run => {
            let _ = writeln!(out, "Usage: pacpin run [options] [repo/]package [arguments...]");
            let _ = writeln!(out, "Runs native/installed, aur/pkg, source-alias/pkg, custom exports, Flatpak, Nix or Pipx targets.");
            let _ = writeln!(out);
            let _ = writeln!(out, "Options:");
            let _ = writeln!(out, "  --dry-run              Preview without downloading or starting the application");
            let _ = writeln!(out, "  --noconfirm, -y        Bypass AUR/Sources build approval prompts");
            let _ = writeln!(out, "  --no-cache             Bypass native archive reuse; build/export caches stay private; Pipx disables reuse");
            let _ = writeln!(out, "  --cleanup=POLICY       temporary, cache, or temporary,cache");
            let _ = writeln!(out, "  --use-ram/--use-storage Keep owned downloads/builds/package trees in RAM/on disk");
            let _ = writeln!(out, "  --installed            Run the installed native package instead of a repository download");
            let _ = writeln!(
                out,
                "  --sandbox              Run in Bubblewrap container (unsandboxed by default)"
            );
            let _ = writeln!(out, "  --bin <name>           Specify exact executable binary name if package provides multiple");
            let _ = writeln!(out, "  --allow-unverified     Proceed even if repository metadata lacks a SHA256 checksum");
            let _ = writeln!(out, "  --sudo | --pkexec      Override configured privilege escalation tool for this run");
            let _ = writeln!(out, "  --                     Pass remaining arguments unchanged to the application");
            let _ = writeln!(out);
            let _ = writeln!(out, "Examples:");
            let _ = writeln!(out, "  pacpin run fastfetch");
            let _ = writeln!(out, "  pacpin run yt-dlp 'https://youtube.com/watch?v=...'");
            let _ = writeln!(out, "  pacpin run ffmpeg -i screencast.mkv output.mp4");
            let _ = writeln!(out, "  pacpin run jq . package.json");
        }
        Mode::Try => {
            let _ = writeln!(out, "Usage: pacpin try [options] [repo/]package [arguments...]");
            let _ = writeln!(out, "Runs native, aur/pkg, source-alias/pkg or declared exports in Bubblewrap; Flatpak owns its sandbox.");
            let _ = writeln!(out);
            let _ = writeln!(out, "Options:");
            let _ = writeln!(out, "  --dry-run              Preview without downloading or starting the application");
            let _ = writeln!(out, "  --noconfirm, -y        Bypass AUR/Sources build approval prompts");
            let _ = writeln!(out, "  --no-cache             Bypass native archive reuse; build/export caches stay private; Pipx disables reuse");
            let _ = writeln!(out, "  --cleanup=POLICY       temporary, cache, or temporary,cache");
            let _ = writeln!(out, "  --use-ram/--use-storage Keep owned downloads/builds/package trees in RAM/on disk");
            let _ = writeln!(out, "  --installed            Test the installed native package with Bubblewrap isolation");
            let _ = writeln!(out, "  --no-sandbox, --bare   Bypass Bubblewrap containerization and run directly on host");
            let _ = writeln!(out, "  --net, --network       Share host network access (unshared by default)");
            let _ = writeln!(out, "  --rw, --write-cwd      Mount current working directory read-write (read-only by default)");
            let _ = writeln!(out, "  --gui                  Grant X11/Wayland and DRI access for graphical applications");
            let _ = writeln!(out, "  --audio                Grant /dev/snd access for audio playback");
            let _ = writeln!(out, "  --bin <name>           Specify exact executable binary name if package provides multiple");
            let _ = writeln!(out, "  --allow-unverified     Proceed even if repository metadata lacks a SHA256 checksum");
            let _ = writeln!(out, "  --sudo | --pkexec      Override configured privilege escalation tool for this run");
            let _ = writeln!(out, "  --                     Pass remaining arguments unchanged to the application");
            let _ = writeln!(out);
            let _ = writeln!(out, "Examples:");
            let _ = writeln!(out, "  pacpin try jq . foo.json");
            let _ = writeln!(out, "  pacpin try --gui flatpak/org.gnome.Calculator");
            let _ = writeln!(out, "  pacpin run nix/ripgrep -i 'foo'  # Nix executes on the host");
        }
    }
}

pub(crate) fn print_mode_help(run_mode: bool) {
    let mode = if run_mode { Mode::Run } else { Mode::Try };
    let mut out = std::io::stdout().lock();
    write_mode_help(mode, &mut out);
}

fn print_missing_target(mode: Mode) {
    let mut err = std::io::stderr().lock();
    match mode {
        Mode::Run => {
            eprintln!(
                "{}",
                "Error: 'pacpin run' requires a package target."
                    .red()
                    .bold()
            );
        }
        Mode::Try => {
            eprintln!(
                "{}",
                "Error: 'pacpin try' requires a package target."
                    .red()
                    .bold()
            );
        }
    }
    write_mode_help(mode, &mut err);
}

fn execute(config: &Config, args: &[String], mode: Mode) {
    let (target, pass_through, options) = parse_args(args, mode);
    let Some(target) = target else {
        print_missing_target(mode);
        exit(1);
    };
    sandbox::cmd_try(config, &target, &pass_through, &options);
}

pub fn cmd_run(config: &Config, args: &[String]) {
    execute(config, args, Mode::Run);
}

pub fn cmd_try(config: &Config, args: &[String]) {
    execute(config, args, Mode::Try);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn run_and_try_keep_distinct_default_isolation() {
        let (_, _, run) = parse_args(&strings(&["jq"]), Mode::Run);
        let (_, _, isolated) = parse_args(&strings(&["jq"]), Mode::Try);
        assert!(run.no_sandbox && run.is_run_mode);
        assert!(!isolated.no_sandbox && !isolated.is_run_mode);
    }

    #[test]
    fn options_and_double_dash_are_preserved() {
        let (target, passthrough, options) = parse_args(
            &strings(&["--net", "--bin", "jq", "extra/jq", "--", "--gui", "-n"]),
            Mode::Try,
        );
        assert_eq!(target.as_deref(), Some("extra/jq"));
        assert_eq!(passthrough, strings(&["--gui", "-n"]));
        assert!(options.share_net);
        assert!(!options.gui);
        assert_eq!(options.bin.as_deref(), Some("jq"));
    }

    #[test]
    fn run_sandbox_override_keeps_arguments() {
        let (target, passthrough, options) = parse_args(
            &strings(&["--sandbox", "fastfetch", "--", "--version"]),
            Mode::Run,
        );
        assert_eq!(target.as_deref(), Some("fastfetch"));
        assert_eq!(passthrough, strings(&["--version"]));
        assert!(!options.no_sandbox && !options.is_run_mode);
        assert!(!options.share_net && !options.rw_cwd);
    }

    #[test]
    fn installed_flag_is_owned_and_double_dash_preserves_program_options() {
        let (target, args, options) = parse_args(&strings(&["--installed", "neofetch", "--", "--installed"]), Mode::Try);
        assert_eq!(target.as_deref(), Some("neofetch"));
        assert!(options.installed && !options.no_sandbox);
        assert_eq!(args, strings(&["--installed"]));
    }
    #[test]
    fn ephemeral_preview_and_cache_flags_do_not_swallow_app_flags_after_delimiter() {
        let (_, args, options) = parse_args(&strings(&["--dry-run", "--no-cache", "app", "--", "--dry-run", "--no-cache"]), Mode::Run);
        assert!(options.dry_run && options.no_cache);
        assert_eq!(args, strings(&["--dry-run", "--no-cache"]));
    }
}
