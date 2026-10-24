//! Explicit directory-inventory and adoption commands; installation stays in pm.
use crate::config::Config;

pub(crate) fn run(config: &Config, args: &[String]) -> Result<(), String> {
    crate::sources::validate_config(config)?;
    match args.first().map(String::as_str) {
        Some("list") if args.len() == 1 => {
            let receipts = crate::sources::directory::inventory(config)?;
            if receipts.is_empty() { println!(":: No managed directory Sources installed."); }
            for receipt in receipts { println!("{}/{} {}  {}", receipt.source, receipt.package, receipt.version, receipt.directory.display()); }
            Ok(())
        }
        Some("adopt") => {
            let mut target = None;
            let mut version = None;
            let mut from = None;
            let mut dry_run = false;
            let mut noconfirm = false;
            let mut index = 1;
            while index < args.len() {
                match args[index].as_str() {
                    "--dry-run" | "-n" => dry_run = true,
                    "--noconfirm" => noconfirm = true,
                    "--version" => { index += 1; version = Some(args.get(index).ok_or("--version needs the installed version")?.as_str()); },
                    "--from" => { index += 1; from = Some(args.get(index).ok_or("--from needs the existing application directory")?.as_str()); },
                    value if value.starts_with("--from=") => from = value.strip_prefix("--from="),
                    value if value.starts_with("--version=") => version = value.strip_prefix("--version="),
                    value if !value.starts_with('-') && target.is_none() => target = Some(value),
                    value => return Err(format!("Unknown directory adoption argument '{value}'")),
                }
                index += 1;
            }
            crate::sources::directory::adopt(config, target.ok_or("Adoption needs alias/package")?, std::path::Path::new(from.ok_or("Adoption needs --from with the existing application directory")?), version.ok_or("Adoption needs --version with the exact installed version")?, dry_run, noconfirm)
        }
        _ => Err("Usage: pin source list | pin source adopt alias/package --from EXISTING_APP --version VERSION [--dry-run] [--noconfirm]".into()),
    }
}
