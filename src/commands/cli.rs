//! Pacman-compatible argument classification shared by command dispatch.

pub fn parse_pacman_cli_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut targets = Vec::new();
    let mut flags = Vec::new();
    let mut after_double_dash = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        if after_double_dash {
            targets.push(arg.clone());
            i += 1;
            continue;
        }

        if arg == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }

        let takes_arg = arg == "--ignore"
            || arg == "--ignoregroup"
            || arg == "--config"
            || arg == "--cachedir"
            || arg == "--root"
            || arg == "-r"
            || arg == "--dbpath"
            || arg == "-b"
            || arg == "--logfile"
            || arg == "--gpgdir"
            || arg == "--hookdir"
            || arg == "--overwrite"
            || arg == "--assume-installed"
            || arg == "--color"
            || arg == "--arch"
            || arg == "--print-format";
        let takes_arg = takes_arg || arg == "--channel";

        if arg == "--wait" {
            flags.push(arg.clone());
            if i + 1 < args.len() && args[i + 1].parse::<u64>().is_ok() {
                flags.push(args[i + 1].clone());
                i += 1;
            }
        } else if takes_arg {
            flags.push(arg.clone());
            if i + 1 < args.len() {
                flags.push(args[i + 1].clone());
                i += 1;
            }
        } else if arg.starts_with("--ignore=")
            || arg.starts_with("--ignoregroup=")
            || arg.starts_with("--config=")
            || arg.starts_with("--cachedir=")
            || arg.starts_with("--root=")
            || arg.starts_with("--dbpath=")
            || arg.starts_with("--logfile=")
            || arg.starts_with("--gpgdir=")
            || arg.starts_with("--hookdir=")
            || arg.starts_with("--overwrite=")
            || arg.starts_with("--assume-installed=")
            || arg.starts_with("--color=")
            || arg.starts_with("--arch=")
            || arg.starts_with("--print-format=")
            || arg.starts_with("--channel=")
            || arg.starts_with("--wait=")
            || arg.starts_with('-')
        {
            flags.push(arg.clone());
        } else {
            targets.push(arg.clone());
        }

        i += 1;
    }

    (targets, flags)
}

pub fn is_upgrade_invocation(args: &[String]) -> bool {
    if args.is_empty() {
        return false;
    }
    let first = &args[0];
    if first == "upgrade" || first == "up" {
        return true;
    }
    if !first.starts_with("-S") {
        return false;
    }

    let (targets, flags_list) = parse_pacman_cli_args(&args[1..]);

    let mut flags = std::collections::HashSet::new();
    for a in std::iter::once(first).chain(flags_list.iter()) {
        if a.starts_with('-') && !a.starts_with("--") {
            for c in a[1..].chars() {
                flags.insert(c);
            }
        }
    }

    // Explicit targets enter the upgrade engine only when a system upgrade is
    // requested (`-Su` / `--sysupgrade`). Plain `-S <pkg>` (even with `-y`)
    // stays on the install path.
    if !(targets.is_empty()
        || flags.contains(&'u')
        || flags_list.iter().any(|a| a == "--sysupgrade"))
    {
        return false;
    }

    if flags.contains(&'s')
        || flags.contains(&'i')
        || flags.contains(&'w')
        || flags.contains(&'p')
        || flags.contains(&'l')
        || flags.contains(&'g')
        || (flags.contains(&'c') && !flags.contains(&'u'))
        || flags_list.iter().any(|a| {
            a == "--search"
                || a == "--info"
                || a == "--downloadonly"
                || a == "--print"
                || a == "--print-uris"
                || a == "--list"
                || a == "--groups"
        })
    {
        return false;
    }

    flags.contains(&'u')
        || flags.contains(&'y')
        || flags_list
            .iter()
            .any(|a| a == "--sysupgrade" || a == "--refresh")
}

/// Pacman's `-Sy` refreshes databases only; it must never enter the upgrade engine.
pub fn is_refresh_only_invocation(args: &[String]) -> bool {
    if !is_upgrade_invocation(args) || matches!(args.first().map(String::as_str), Some("upgrade" | "up")) {
        return false;
    }
    let first = &args[0];
    let (_, flags) = parse_pacman_cli_args(&args[1..]);
    !std::iter::once(first).chain(flags.iter()).any(|arg| {
        arg == "--sysupgrade"
            || (arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains('u'))
    })
}

pub fn pacman_files_needs_sudo(args: &[String]) -> bool {
    let (_, flags) = parse_pacman_cli_args(args);
    flags.iter().any(|a| {
        if a == "--refresh" {
            true
        } else if a.starts_with('-') && !a.starts_with("--") {
            a.contains('y')
        } else {
            false
        }
    })
}
