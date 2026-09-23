// pacpin - Declarative Package Resolver & Upgrade Engine for Arch Linux / CachyOS
// Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

pub fn is_safe_pkg_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.starts_with('-')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '+' || c == '-' || c == '@')
}

pub fn is_safe_version(ver: &str) -> bool {
    !ver.is_empty()
        && !ver.contains('/')
        && !ver.contains('\\')
        && !ver.contains("..")
        && ver
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '+' || c == '-' || c == ':' || c == '~')
}

pub const KERNEL_PACKAGES: &[&str] = &[
    "linux",
    "linux-lts",
    "linux-zen",
    "linux-hardened",
    "linux-cachyos",
    "linux-cachyos-lts",
    "linux-cachyos-server",
    "linux-cachyos-bore",
    "linux-xanmod",
    "linux-rt",
    "linux-rt-lts",
    "linux-tkg",
    "linux-bochs",
];

pub fn is_kernel_package(name: &str) -> bool {
    if name == "linux-firmware" || name.starts_with("linux-firmware-") {
        return false;
    }
    KERNEL_PACKAGES.iter().any(|k| {
        if *k == "linux" {
            name == "linux" || name == "linux-headers" || name == "linux-docs"
        } else {
            name == *k || name.starts_with(&format!("{}-", k))
        }
    })
}

pub fn is_safe_pattern(pat: &str) -> bool {
    !pat.is_empty()
        && !pat.starts_with('.')
        && !pat.starts_with('-')
        && !pat.contains('/')
        && !pat.contains('\\')
        && !pat.contains("..")
        && pat.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || c == '.'
                || c == '_'
                || c == '+'
                || c == '-'
                || c == '@'
                || c == '*'
                || c == '?'
                || c == '['
                || c == ']'
        })
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn is_executable_file(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode();
            return (mode & 0o111) != 0;
        }
        false
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Validates whether an install or removal target is safe.
/// Supports both bare package names ("ollama") and repository-qualified targets ("extra/ollama").
pub fn is_safe_install_target(target: &str) -> bool {
    let t = target.trim();
    if t.is_empty() || t.starts_with('-') || t.starts_with('.') {
        return false;
    }
    match t.split_once('/') {
        Some((repo, pkg)) => {
            !repo.is_empty()
                && !pkg.is_empty()
                && is_safe_pkg_name(repo)
                && is_safe_pkg_name(pkg)
                && !pkg.contains('/')
        }
        None => is_safe_pkg_name(t),
    }
}

/// Strips C0 and C1 control characters and ANSI/VT100/OSC escape sequences from untrusted display text,
/// while leaving standard UTF-8 characters (including wide/emoji glyphs) and basic whitespace intact.
pub fn sanitize_display_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if let Some(&next) = chars.peek() {
                if next == '[' {
                    chars.next();
                    // CSI sequence: terminates on a final byte in '@'..='~' (0x40..=0x7E)
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                } else if next == ']' {
                    chars.next();
                    // OSC sequence: terminates on BEL (\x07) or ST (\x1b\\)
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c == '\x07' {
                            break;
                        } else if c == '\x1b' {
                            if let Some(&'\\') = chars.peek() {
                                chars.next();
                            }
                            break;
                        }
                    }
                } else {
                    // Two-character escape sequence (e.g. ESC M, ESC 7, ESC =)
                    chars.next();
                }
            }
            continue;
        }
        // Allow tab, newline, carriage return, but reject other ASCII control codes (0x00..0x1F)
        if (ch as u32) < 0x20 && ch != '\t' && ch != '\n' && ch != '\r' {
            continue;
        }
        // Reject DEL (0x7F) and C1 control codes (0x80..=0x9F)
        if (0x7F..=0x9F).contains(&(ch as u32)) {
            continue;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_pkg_name() {
        assert!(is_safe_pkg_name("linux"));
        assert!(is_safe_pkg_name("linux-cachyos"));
        assert!(is_safe_pkg_name("lib32-glibc"));
        assert!(is_safe_pkg_name("python@3.12"));
        assert!(is_safe_pkg_name("gcc-libs"));

        assert!(!is_safe_pkg_name(""));
        assert!(!is_safe_pkg_name("../etc/passwd"));
        assert!(!is_safe_pkg_name("foo/bar"));
        assert!(!is_safe_pkg_name("foo\\bar"));
        assert!(!is_safe_pkg_name("foo;rm"));
        assert!(!is_safe_pkg_name("foo*"));
        assert!(!is_safe_pkg_name("pkg name"));
        assert!(!is_safe_pkg_name("-badname"));
        assert!(!is_safe_pkg_name(".badname"));
    }

    #[test]
    fn test_is_safe_install_target() {
        assert!(is_safe_install_target("ollama"));
        assert!(is_safe_install_target("extra/ollama"));
        assert!(is_safe_install_target("cachyos/linux-cachyos"));
        assert!(is_safe_install_target("lib32-glibc"));

        assert!(!is_safe_install_target(""));
        assert!(!is_safe_install_target("-S"));
        assert!(!is_safe_install_target("--config=/tmp/bad"));
        assert!(!is_safe_install_target("-bad"));
        assert!(!is_safe_install_target(".bad"));
        assert!(!is_safe_install_target("extra/"));
        assert!(!is_safe_install_target("/ollama"));
        assert!(!is_safe_install_target("extra/../etc/passwd"));
        assert!(!is_safe_install_target("extra/sub/pkg"));
        assert!(!is_safe_install_target("ollama;rm -rf /"));
    }

    #[test]
    fn test_sanitize_display_text() {
        assert_eq!(sanitize_display_text("Normal text"), "Normal text");
        assert_eq!(sanitize_display_text("Line 1\nLine 2"), "Line 1\nLine 2");
        assert_eq!(sanitize_display_text("Emojis: 🚀 and symbols: — ✔"), "Emojis: 🚀 and symbols: — ✔");

        // ANSI color escapes
        assert_eq!(sanitize_display_text("\x1b[31mRed Text\x1b[0m"), "Red Text");
        // Cursor movement / clear screen
        assert_eq!(sanitize_display_text("\x1b[2J\x1b[HClear"), "Clear");
        // OSC hyperlink
        assert_eq!(sanitize_display_text("\x1b]8;;https://malicious.link\x07Click Here\x1b]8;;\x07"), "Click Here");
        // C0 control chars
        assert_eq!(sanitize_display_text("Bell\x07 and Backspace\x08"), "Bell and Backspace");
    }

    #[test]
    fn test_is_safe_version() {
        assert!(is_safe_version("6.13.4-1"));
        assert!(is_safe_version("1:2.35.2-2.1"));
        assert!(!is_safe_version("1.0; rm -rf /"));
        assert!(!is_safe_version(""));
    }

    #[test]
    fn test_shell_quote() {
        assert_eq!(shell_quote("simple"), "'simple'");
        assert_eq!(shell_quote("with space"), "'with space'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("; rm -rf /"), "'; rm -rf /'");
    }

    #[test]
    fn test_is_kernel_package() {
        assert!(is_kernel_package("linux"));
        assert!(is_kernel_package("linux-headers"));
        assert!(is_kernel_package("linux-lts"));
        assert!(is_kernel_package("linux-zen"));
        assert!(is_kernel_package("linux-cachyos"));
        assert!(is_kernel_package("linux-cachyos-bore"));
        assert!(is_kernel_package("linux-xanmod"));
        assert!(!is_kernel_package("linux-firmware"));
        assert!(!is_kernel_package("util-linux"));
    }

    #[test]
    fn test_is_safe_pattern() {
        assert!(is_safe_pattern("linux*"));
        assert!(is_safe_pattern("linux-*"));
        assert!(is_safe_pattern("*nvidia*"));
        assert!(is_safe_pattern("mesa?"));
        assert!(!is_safe_pattern(""));
        assert!(!is_safe_pattern("; rm -rf /"));
        assert!(!is_safe_pattern("../foo"));
        assert!(!is_safe_pattern("/etc/*"));
    }
}

