//! Normalize bounded manager output without owning the terminal or child process.
#[derive(Default)]
pub(super) struct Observer {
    pub(super) line: Vec<u8>,
    pub(super) escape: u8,
    pub(super) ready: bool,
    pub(super) latest: String,
    pub(super) interrupted: bool,
}
impl Observer {
    pub(super) fn feed(&mut self, bytes: &[u8], on_ready: &mut impl FnMut()) {
        self.feed_observed(bytes, on_ready, &mut |_| {});
    }
    pub(super) fn feed_observed(&mut self, bytes: &[u8], on_ready: &mut impl FnMut(), on_line: &mut impl FnMut(&str)) {
        for &byte in bytes {
            // Consume CSI and OSC sequences even when split across reads.
            match self.escape {
                1 => {
                    self.escape = match byte {
                        b'[' => 2,
                        b']' => 3,
                        _ => 0,
                    };
                    continue;
                }
                2 => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.escape = 0;
                    }
                    continue;
                }
                3 => {
                    if byte == 7 {
                        self.escape = 0;
                    } else if byte == 27 {
                        self.escape = 4;
                    }
                    continue;
                }
                4 => {
                    self.escape = if byte == b'\\' { 0 } else { 3 };
                    continue;
                }
                _ => {}
            }
            match byte {
                27 => self.escape = 1,
                b'\r' | b'\n' => {
                    let line = String::from_utf8_lossy(&self.line).trim().to_owned();
                    if !line.is_empty() {
                        self.interrupted |= line == "Interrupt signal received";
                        on_line(&line);
                        if !self.ready && downloads_finished(&line) {
                            self.ready = true;
                            on_ready();
                        }
                        self.latest = line;
                    }
                    self.line.clear();
                }
                byte if byte >= 32 && byte != 127 => {
                    if self.line.len() < 8192 {
                        self.line.push(byte);
                    }
                    // A prompt/progress update need not end with a newline.
                    if !self.ready && downloads_finished(&String::from_utf8_lossy(&self.line)) {
                        self.ready = true;
                        on_ready();
                    }
                }
                _ => {}
            }
        }
    }
    pub(super) fn display(&self) -> String {
        if self.line.is_empty() {
            self.latest.clone()
        } else {
            String::from_utf8_lossy(&self.line).into_owned()
        }
    }
}

pub(super) fn downloads_finished(line: &str) -> bool {
    [
        "checking keys in keyring",
        "checking keyring",
        "checking package integrity",
        "checking for file conflicts",
        "Processing package changes",
        "there is nothing to do",
    ]
    .iter()
    .any(|marker| line.contains(marker))
}

pub(super) fn clean_text(text: &str) -> String {
    let mut observer = Observer::default();
    observer.feed(text.as_bytes(), &mut || {});
    observer.display()
}
