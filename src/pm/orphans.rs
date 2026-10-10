//! Typed native orphan removal; terminal/process ownership stays shared.

pub fn remove_orphans(selected: &[String], privilege_tool: &str) -> Result<(), String> {
    if selected.is_empty() { return Ok(()); }
    if let Some(name) = selected.iter().find(|name| !crate::utils::is_safe_pkg_name(name)) {
        return Err(format!("Refusing unsafe orphan target '{name}'"));
    }
    crate::config::validate_privilege_tool(privilege_tool)?;
    super::check_lock()?;
    let mut command = crate::privilege::command(privilege_tool);
    // The selector already enumerates orphan dependencies. Recursive removal
    // would silently add unchecked packages to the approved set.
    command.args(["pacman", "-Rn", "--noconfirm"])
        .args(crate::progress::pacman_progress_args()).arg("--").args(selected);
    let status = crate::progress::run(&mut command, crate::progress::Manager::Orphans, "Removing packages", || {})
        .map_err(|error| format!("Orphan removal execution failed: {error}"))?;
    if !status.success() { return Err(format!("Orphan removal exited with status {status}")); }
    crate::progress::completed(crate::progress::Manager::Orphans);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn orphan_removal_skips_empty_work_and_rejects_unsafe_targets_before_execution() {
        assert!(super::remove_orphans(&[], "sudo").is_ok());
        assert!(super::remove_orphans(&["--nodeps".into()], "sudo").is_err());
        assert!(super::remove_orphans(&["../pkg".into()], "sudo").is_err());
        assert!(super::remove_orphans(&["valid".into()], "sudo --bad").is_err());
    }
}
