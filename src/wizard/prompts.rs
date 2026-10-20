//! Line-based input has one EOF/cancellation contract; no raw terminal mode.
use std::io::{self, BufRead, Write};

#[derive(Debug)]
pub(super) enum Error {
    Cancelled,
    Io(io::Error),
}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
pub(super) type Result<T> = std::result::Result<T, Error>;

pub(super) struct Prompts<R, W> {
    input: R,
    output: W,
}
impl<R: BufRead, W: Write> Prompts<R, W> {
    pub(super) fn new(input: R, output: W) -> Self {
        Self { input, output }
    }

    pub(super) fn say(&mut self, text: &str) -> Result<()> {
        for line in crate::ui::wrap_field(text, "  ", "  ", crate::ui::get_terminal_width()) {
            writeln!(self.output, "{line}")?;
        }
        Ok(())
    }

    pub(super) fn section(&mut self, title: &str) -> Result<()> {
        writeln!(self.output)?;
        self.say(title)
    }

    fn line(&mut self) -> Result<String> {
        self.output.flush()?;
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            return Err(Error::Cancelled);
        }
        let line = line.trim().to_string();
        if line.eq_ignore_ascii_case("q") || line.eq_ignore_ascii_case("quit") {
            return Err(Error::Cancelled);
        }
        Ok(line)
    }

    pub(super) fn yes_no(&mut self, label: &str, default: bool) -> Result<bool> {
        loop {
            self.say(&format!(
                "{label} {}",
                if default { "[Y/n]" } else { "[y/N]" }
            ))?;
            write!(self.output, "  > ")?;
            match self.line()?.to_ascii_lowercase().as_str() {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => self.say("Enter yes or no, or q to cancel setup.")?,
            }
        }
    }

    pub(super) fn value<T>(
        &mut self,
        label: &str,
        default: &str,
        validate: impl Fn(&str) -> std::result::Result<T, String>,
    ) -> Result<T> {
        loop {
            self.say(&format!("{label} [{default}]"))?;
            write!(self.output, "  > ")?;
            let line = self.line()?;
            let value = if line.is_empty() { default } else { &line };
            match validate(value) {
                Ok(value) => return Ok(value),
                Err(error) => self.say(&error)?,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_answers_and_never_treats_eof_as_acceptance() {
        let mut prompts = Prompts::new(&b"wrong\nyes\nq\n"[..], Vec::new());
        assert!(prompts.yes_no("Enable?", false).unwrap());
        assert!(matches!(
            prompts.yes_no("Save?", true),
            Err(Error::Cancelled)
        ));
        let mut prompts = Prompts::new(&b""[..], Vec::new());
        assert!(matches!(
            prompts.yes_no("Save?", true),
            Err(Error::Cancelled)
        ));
    }
    #[test]
    fn rejects_invalid_values_instead_of_silently_using_defaults() {
        let mut prompts = Prompts::new(&b"invalid\n9\n"[..], Vec::new());
        let days = prompts
            .value("Days", "3", |value| {
                value
                    .parse::<u32>()
                    .map_err(|_| "Enter a nonnegative integer".into())
            })
            .unwrap();
        assert_eq!(days, 9);
    }
}
