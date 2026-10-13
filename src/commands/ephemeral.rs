//! CLI handling for ephemeral host execution and sandboxed execution.

use std::process::exit;

use colored::Colorize;

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

fn print_missing_target(mode: Mode) {
    match mode {
        Mode::Run => {
            eprintln!(
                "{}",
                "Error: 'pacpin run' requires a package target."
                    .red()
                    .bold()
            );
            eprintln!("Usage: pacpin run [options] [repo/]package [arguments...]");
            eprintln!("Runs a native repository or installed package, Flatpak, Nix, or Pipx target on the host.");
            eprintln!();
            eprintln!("Options:");
            eprintln!("  --dry-run              Preview without downloading or starting the application");
            eprintln!("  --no-cache             Native: bypass archive cache; Pipx: bypass environment reuse");
            eprintln!("  --use-ram/--use-storage Keep repository downloads and extracted files in RAM/on disk");
            eprintln!("  --installed            Run the installed native package instead of a repository download");
            eprintln!(
                "  --sandbox              Run in Bubblewrap container (unsandboxed by default)"
            );
            eprintln!("  --bin <name>           Specify exact executable binary name if package provides multiple");
            eprintln!("  --allow-unverified     Proceed even if repository metadata lacks a SHA256 checksum");
            eprintln!();
            eprintln!("Examples:");
            eprintln!("  pacpin run fastfetch");
            eprintln!("  pacpin run yt-dlp 'https://youtube.com/watch?v=...'");
            eprintln!("  pacpin run ffmpeg -i screencast.mkv output.mp4");
            eprintln!("  pacpin run jq . package.json");
        }
        Mode::Try => {
            eprintln!(
                "{}",
                "Error: 'pacpin try' requires a package target."
                    .red()
                    .bold()
            );
            eprintln!("Usage: pacpin try [options] [repo/]package [arguments...]");
            eprintln!("Runs native Arch packages in a Bubblewrap container; Flatpak uses its own sandbox.");
            eprintln!();
            eprintln!("Options:");
            eprintln!("  --dry-run              Preview without downloading or starting the application");
            eprintln!("  --no-cache             Native: bypass archive cache; Pipx: bypass environment reuse");
            eprintln!("  --use-ram/--use-storage Keep repository downloads and extracted files in RAM/on disk");
            eprintln!("  --installed            Test the installed native package with Bubblewrap isolation");
            eprintln!("  --no-sandbox, --bare   Bypass Bubblewrap containerization and run directly on host");
            eprintln!("  --net, --network       Share host network access (unshared by default)");
            eprintln!("  --rw, --write-cwd      Mount current working directory read-write (read-only by default)");
            eprintln!("  --gui                  Grant X11/Wayland and DRI access for graphical applications");
            eprintln!("  --audio                Grant /dev/snd access for audio playback");
            eprintln!("  --bin <name>           Specify exact executable binary name if package provides multiple");
            eprintln!("  --allow-unverified     Proceed even if repository metadata lacks a SHA256 checksum");
            eprintln!();
            eprintln!("Examples:");
            eprintln!("  pacpin try jq . foo.json");
            eprintln!("  pacpin try --gui flatpak/org.gnome.Calculator");
            eprintln!("  pacpin run nix/ripgrep -i 'foo'  # Nix executes on the host");
        }
    }
}

fn execute(args: &[String], mode: Mode) {
    let (target, pass_through, options) = parse_args(args, mode);
    let Some(target) = target else {
        print_missing_target(mode);
        exit(1);
    };
    sandbox::cmd_try(&target, &pass_through, &options);
}

pub fn cmd_run(args: &[String]) {
    execute(args, Mode::Run);
}

pub fn cmd_try(args: &[String]) {
    execute(args, Mode::Try);
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
