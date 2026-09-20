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

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
}
