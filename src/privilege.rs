//! Authentication belongs to one pacpin run, on the invoking terminal.
use std::io::{self, IsTerminal};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

static FRESH_SUDO: AtomicBool = AtomicBool::new(false);
static SESSION: OnceLock<Result<(), String>> = OnceLock::new();

pub fn initialize(tool: &str, args: &[String]) {
    FRESH_SUDO.store(
        fresh_session(tool, io::stdin().is_terminal(), args),
        Ordering::Relaxed,
    );
}

fn fresh_session(tool: &str, terminal: bool, args: &[String]) -> bool {
    tool == "sudo" && terminal && !args.iter().any(|arg| arg == "--dry-run" || arg == "-n")
}

/// Called only when an approved privileged operation or AUR install will run.
/// The first actual sudo command authenticates; later commands share its tty.
pub fn prepare(tool: &str) -> io::Result<()> {
    if tool != "sudo" || !FRESH_SUDO.load(Ordering::Relaxed) {
        return Ok(());
    }
    SESSION
        .get_or_init(|| {
            // Reset only this terminal's ticket, never all of the user's sessions.
            match Command::new("sudo").arg("-k").status() {
                Ok(status) if status.success() => {}
                Ok(status) => {
                    return Err(format!(
                        "Cannot start a fresh sudo session (exit {status})."
                    ));
                }
                Err(error) => {
                    return Err(format!("Cannot start a fresh sudo session: {error}"));
                }
            }
            // Long AUR builds/downloads can exceed the usual sudo timeout. Renew
            // an authenticated ticket without prompting or handling a password.
            // Before authentication, -n simply fails. The worker dies with pacpin.
            let _ = std::thread::Builder::new()
                .name("sudo-session".into())
                .spawn(|| loop {
                    std::thread::sleep(Duration::from_secs(60));
                    let _ = crate::execution::run_tracked_command(
                        renewal_command()
                            // Keep the same input tty for sudo's timestamp lookup.
                            // -n guarantees this validation cannot read a password.
                            .stdin(Stdio::inherit())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null()),
                    );
                });
            Ok(())
        })
        .as_ref()
        .map(|_| ())
        .map_err(|message| io::Error::other(message.clone()))
}

fn renewal_command() -> Command {
    let mut command = Command::new("sudo");
    command.args(["-n", "-v"]);
    command
}

/// A noninteractive validation can show a phase immediately when no password
/// is needed. Unknown privilege tools retain the deferred-prompt display.
pub fn authenticated(tool: &std::ffi::OsStr) -> bool {
    tool == "sudo" && crate::execution::run_tracked_command(
        renewal_command()
            .stdin(Stdio::inherit())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    ).is_ok_and(|status| status.success())
}

pub fn command(tool: &str) -> Command {
    prepared_command(tool, prepare(tool))
}

fn prepared_command(tool: &str, prepared: io::Result<()>) -> Command {
    let mut command = Command::new(tool);
    if let Err(error) = prepared {
        eprintln!("Error: {error}");
        let errno = error.raw_os_error().unwrap_or(libc::EACCES);
        // SAFETY: this hook only returns a captured OS error, without locks,
        // allocation, or I/O. Refusing spawn preserves the caller's normal
        // error handling, workspace cleanup and terminal restoration.
        unsafe {
            command.pre_exec(move || Err(io::Error::from_raw_os_error(errno)));
        }
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_authentication_is_scoped_to_interactive_sudo_and_real_operations() {
        assert!(fresh_session("sudo", true, &["-Syu".into()]));
        for tool in ["pkexec", "doas"] {
            assert!(!fresh_session(tool, true, &["-Syu".into()]));
        }
        assert!(!fresh_session("sudo", false, &["-Syu".into()]));
        for flag in ["--dry-run", "-n"] {
            assert!(!fresh_session("sudo", true, &["-Syu".into(), flag.into()]));
        }
    }

    #[test]
    fn renewal_cannot_prompt_or_execute_a_privileged_package_command() {
        let command = renewal_command();
        assert_eq!(command.get_program(), "sudo");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["-n", "-v"]);
    }

    #[test]
    fn failed_session_preparation_refuses_execution_without_exiting_the_caller() {
        let error = prepared_command("sh", Err(io::Error::from_raw_os_error(libc::EACCES)))
            .args(["-c", "exit 99"])
            .status()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}
