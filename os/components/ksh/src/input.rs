//! 有界串口行编辑；历史属于会话，溢出或无效字节使整行失效。
use crate::parser::{COMMANDS, LINE_MAX};

pub const HISTORY_MAX: usize = 8;
pub const VIEW_WIDTH: usize = 64;

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    None,
    Echo(u8),
    Refresh,
    Complete,
    Clear,
    Submit,
    Overflow,
    Invalid,
    Cancel,
    Exit,
}

pub struct Input {
    bytes: [u8; LINE_MAX],
    len: usize,
    cursor: usize,
    overflow: bool,
    invalid: bool,
    reset: bool,
    skip_lf: bool,
    escape: u8,
    parameter: u16,
    history: [[u8; LINE_MAX]; HISTORY_MAX],
    history_lens: [usize; HISTORY_MAX],
    history_len: usize,
    browsing: Option<usize>,
    draft: [u8; LINE_MAX],
    draft_len: usize,
    draft_cursor: usize,
}

impl Input {
    pub const fn new() -> Self {
        Self {
            bytes: [0; LINE_MAX],
            len: 0,
            cursor: 0,
            overflow: false,
            invalid: false,
            reset: false,
            skip_lf: false,
            escape: 0,
            parameter: 0,
            history: [[0; LINE_MAX]; HISTORY_MAX],
            history_lens: [0; HISTORY_MAX],
            history_len: 0,
            browsing: None,
            draft: [0; LINE_MAX],
            draft_len: 0,
            draft_cursor: 0,
        }
    }

    pub fn line(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    // A horizontal window avoids wrapping when editing long lines on an
    // ordinary 80-column serial terminal. Editing still addresses the full line.
    pub fn view(&self) -> (&[u8], usize) {
        let start = self.cursor.saturating_sub(VIEW_WIDTH);
        let end = self.len.min(start + VIEW_WIDTH);
        (&self.bytes[start..end], end - self.cursor)
    }

    pub fn history(&self) -> impl Iterator<Item = &[u8]> {
        self.history[..self.history_len]
            .iter()
            .zip(&self.history_lens)
            .map(|(line, &len)| &line[..len])
    }

    pub fn remember(&mut self) {
        let line = self.line();
        if line.trim_ascii().is_empty() || self.history().last() == Some(line) {
            return;
        }
        if self.history_len == HISTORY_MAX {
            self.history.copy_within(1..HISTORY_MAX, 0);
            self.history_lens.copy_within(1..HISTORY_MAX, 0);
            self.history_len -= 1;
        }
        self.history[self.history_len][..self.len].copy_from_slice(&self.bytes[..self.len]);
        self.history_lens[self.history_len] = self.len;
        self.history_len += 1;
    }

    fn recall(&mut self, older: bool) -> Event {
        if self.history_len == 0 {
            return Event::None;
        }
        let index = match (self.browsing, older) {
            (None, true) => {
                self.draft[..self.len].copy_from_slice(&self.bytes[..self.len]);
                self.draft_len = self.len;
                self.draft_cursor = self.cursor;
                self.history_len - 1
            }
            (Some(0), true) | (None, false) => return Event::None,
            (Some(index), true) => index - 1,
            (Some(index), false) if index + 1 < self.history_len => index + 1,
            (Some(_), false) => {
                self.len = self.draft_len;
                self.bytes[..self.len].copy_from_slice(&self.draft[..self.len]);
                self.cursor = self.draft_cursor;
                self.browsing = None;
                return Event::Refresh;
            }
        };
        self.len = self.history_lens[index];
        self.bytes[..self.len].copy_from_slice(&self.history[index][..self.len]);
        self.cursor = self.len;
        self.browsing = Some(index);
        Event::Refresh
    }

    fn remove(&mut self, start: usize, end: usize) -> Event {
        if start == end {
            return Event::None;
        }
        self.bytes.copy_within(end..self.len, start);
        self.len -= end - start;
        self.cursor = start;
        Event::Refresh
    }

    /// Complete only the first, unquoted command word at its end. The returned
    /// candidates borrow the static inventory, never a registry or provider.
    pub fn complete(&mut self) -> ([&'static [u8]; COMMANDS.len()], usize) {
        let mut matches = [b"".as_slice(); COMMANDS.len()];
        if self.overflow || self.invalid || self.cursor != self.len {
            return (matches, 0);
        }
        let start = self
            .line()
            .iter()
            .position(|b| *b != b' ')
            .unwrap_or(self.len);
        let prefix = &self.bytes[start..self.len];
        if prefix.iter().any(|b| !b.is_ascii_alphanumeric()) {
            return (matches, 0);
        }
        let mut count = 0;
        for &(name, _) in COMMANDS {
            if name.starts_with(prefix) {
                matches[count] = name;
                count += 1;
            }
        }
        if count == 0 {
            return (matches, 0);
        }
        let mut common = matches[0].len();
        for name in &matches[1..count] {
            common = matches[0][..common]
                .iter()
                .zip(name.iter())
                .take_while(|(a, b)| a == b)
                .count();
        }
        let end = start + common;
        if end + usize::from(count == 1) <= LINE_MAX {
            self.bytes[start..end].copy_from_slice(&matches[0][..common]);
            self.len = end;
            if count == 1 {
                self.bytes[self.len] = b' ';
                self.len += 1;
            }
            self.cursor = self.len;
        }
        (matches, count)
    }

    fn escape_byte(&mut self, byte: u8) -> Event {
        match self.escape {
            1 => {
                self.escape = match byte {
                    b'[' => 2,
                    b'O' => 3,
                    _ => 0,
                };
                self.parameter = 0;
                return Event::None;
            }
            2 if byte.is_ascii_digit() => {
                self.parameter = self
                    .parameter
                    .saturating_mul(10)
                    .saturating_add(u16::from(byte - b'0'));
                return Event::None;
            }
            2 if byte == b';' => {
                self.escape = 4;
                return Event::None;
            }
            2..=4 if !(0x40..=0x7e).contains(&byte) => return Event::None,
            _ => {}
        }
        let escape = self.escape;
        self.escape = 0;
        if escape == 4 {
            return Event::None;
        }
        match (byte, self.parameter) {
            (b'A', 0 | 1) => self.recall(true),
            (b'B', 0 | 1) => self.recall(false),
            (b'C', 0 | 1) => {
                self.cursor = (self.cursor + 1).min(self.len);
                Event::Refresh
            }
            (b'D', 0 | 1) => {
                self.cursor = self.cursor.saturating_sub(1);
                Event::Refresh
            }
            (b'H', 0 | 1) | (b'~', 1 | 7) => {
                self.cursor = 0;
                Event::Refresh
            }
            (b'F', 0 | 1) | (b'~', 4 | 8) => {
                self.cursor = self.len;
                Event::Refresh
            }
            (b'~', 3) => self.remove(self.cursor, (self.cursor + 1).min(self.len)),
            _ => Event::None,
        }
    }

    pub fn feed(&mut self, byte: u8) -> Event {
        if self.reset {
            self.len = 0;
            self.cursor = 0;
            self.overflow = false;
            self.invalid = false;
            self.browsing = None;
            self.escape = 0;
            self.reset = false;
        }
        if self.skip_lf {
            self.skip_lf = false;
            if byte == b'\n' {
                return Event::None;
            }
        }
        match byte {
            b'\r' | b'\n' => {
                self.skip_lf = byte == b'\r';
                self.reset = true;
                return if self.overflow {
                    Event::Overflow
                } else if self.invalid {
                    Event::Invalid
                } else {
                    Event::Submit
                };
            }
            3 => {
                self.reset = true;
                return Event::Cancel;
            }
            _ => {}
        }
        if self.overflow || self.invalid {
            return Event::None;
        }
        if byte == 27 {
            self.escape = 1;
            return Event::None;
        }
        if self.escape != 0 {
            return self.escape_byte(byte);
        }
        match byte {
            1 => {
                self.cursor = 0;
                Event::Refresh
            } // Ctrl-A
            2 => {
                self.cursor = self.cursor.saturating_sub(1);
                Event::Refresh
            }
            5 => {
                self.cursor = self.len;
                Event::Refresh
            } // Ctrl-E
            6 => {
                self.cursor = (self.cursor + 1).min(self.len);
                Event::Refresh
            }
            4 if self.len == 0 => Event::Exit,
            4 => self.remove(self.cursor, (self.cursor + 1).min(self.len)),
            8 | 127 => self.remove(self.cursor.saturating_sub(1), self.cursor),
            b'\t' => Event::Complete,
            11 => self.remove(self.cursor, self.len), // Ctrl-K
            12 => Event::Clear,                       // Ctrl-L
            14 => self.recall(false),                 // Ctrl-N
            16 => self.recall(true),                  // Ctrl-P
            21 => self.remove(0, self.cursor),        // Ctrl-U
            23 => {
                // Ctrl-W: delete whitespace and the previous word.
                let mut start = self.cursor;
                while start > 0 && self.bytes[start - 1] == b' ' {
                    start -= 1;
                }
                while start > 0 && self.bytes[start - 1] != b' ' {
                    start -= 1;
                }
                self.remove(start, self.cursor)
            }
            32..=126 => {
                if self.len == LINE_MAX {
                    self.overflow = true;
                    return Event::None;
                }
                let append = self.cursor == self.len && self.len < VIEW_WIDTH;
                self.bytes
                    .copy_within(self.cursor..self.len, self.cursor + 1);
                self.bytes[self.cursor] = byte;
                self.cursor += 1;
                self.len += 1;
                if append {
                    Event::Echo(byte)
                } else {
                    Event::Refresh
                }
            }
            128..=255 => {
                self.invalid = true;
                Event::None
            }
            _ => Event::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(input: &mut Input, bytes: &[u8]) -> Event {
        bytes
            .iter()
            .map(|&b| input.feed(b))
            .last()
            .unwrap_or(Event::None)
    }

    fn submit(input: &mut Input, line: &[u8]) {
        feed(input, line);
        assert_eq!(input.feed(b'\n'), Event::Submit);
        input.remember();
    }

    #[test]
    fn overflow_and_invalid_input_discard_whole_line_and_recover() {
        let mut input = Input::new();
        feed(&mut input, &[b'x'; LINE_MAX]);
        assert_eq!(input.line().len(), LINE_MAX);
        assert_eq!(input.feed(b'y'), Event::None);
        feed(&mut input, b"\x1b[D\x7f\x15");
        assert_eq!(input.feed(b'\n'), Event::Overflow);
        submit(&mut input, b"help");
        assert_eq!(input.line(), b"help");
        feed(&mut input, b"exit\xff");
        assert_eq!(input.feed(b'\n'), Event::Invalid);
        submit(&mut input, b"echo recovered");
        assert_eq!(
            input.history().collect::<std::vec::Vec<_>>(),
            [b"help".as_slice(), b"echo recovered"]
        );
    }

    #[test]
    fn insertion_delete_and_terminal_cursor_sequences() {
        let mut input = Input::new();
        feed(&mut input, b"echo ac\x1b[Db");
        assert_eq!(input.line(), b"echo abc");
        feed(&mut input, b"\x1b[3~");
        assert_eq!(input.line(), b"echo ab");
        feed(&mut input, b"\x1b[H\x04E\x1b[F!");
        assert_eq!(input.line(), b"Echo ab!");
        feed(&mut input, b"\x1b[1~e\x1b[4~\x7f");
        assert_eq!(input.line(), b"eEcho ab");
        feed(&mut input, b"\x1bOH\x04\x1bOF");
        assert_eq!(input.line(), b"Echo ab");
    }

    #[test]
    fn control_keys_remove_prefix_suffix_and_word() {
        let mut input = Input::new();
        feed(&mut input, b"echo one two  \x17");
        assert_eq!(input.line(), b"echo one ");
        feed(&mut input, b"\x02\x0b");
        assert_eq!(input.line(), b"echo one");
        feed(&mut input, b"\x01\x06\x06\x15");
        assert_eq!(input.line(), b"ho one");
        assert_eq!(input.cursor, 0);
        feed(&mut input, b"\x05\x08");
        assert_eq!(input.line(), b"ho on");
        assert_eq!(input.feed(12), Event::Clear);
        assert_eq!(input.line(), b"ho on");
    }

    #[test]
    fn history_restores_draft_and_does_not_mutate_stored_lines() {
        let mut input = Input::new();
        submit(&mut input, b"echo first");
        submit(&mut input, b"echo second");
        feed(&mut input, b"draft\x02");
        feed(&mut input, b"\x1b[A");
        assert_eq!(input.line(), b"echo second");
        feed(&mut input, b"!\x10");
        assert_eq!(input.line(), b"echo first");
        feed(&mut input, b"\x10");
        assert_eq!(input.line(), b"echo first");
        feed(&mut input, b"\x0e\x1b[B");
        assert_eq!(input.line(), b"draft");
        assert_eq!(input.cursor, 4);
        assert_eq!(input.history().last(), Some(b"echo second".as_slice()));
    }

    #[test]
    fn history_is_bounded_skips_blank_and_consecutive_duplicates() {
        let mut input = Input::new();
        for line in [
            b" ".as_slice(),
            b"echo 0",
            b"echo 0",
            b"echo 1",
            b"echo 2",
            b"echo 3",
            b"echo 4",
            b"echo 5",
            b"echo 6",
            b"echo 7",
            b"echo 8",
        ] {
            submit(&mut input, line);
        }
        assert_eq!(input.history_len, HISTORY_MAX);
        assert_eq!(input.history().next(), Some(b"echo 1".as_slice()));
        assert_eq!(input.history().last(), Some(b"echo 8".as_slice()));
        feed(&mut input, b"cancelled\x03\x1b[A");
        assert_eq!(input.line(), b"echo 8");
    }

    #[test]
    fn completion_is_command_only_and_preserves_ambiguous_prefixes() {
        let mut input = Input::new();
        feed(&mut input, b"  ins");
        assert_eq!(input.complete().1, 1);
        assert_eq!(input.line(), b"  inspect ");
        assert_eq!(input.complete().1, 0);
        assert_eq!(input.feed(b'\n'), Event::Submit);
        feed(&mut input, b"e");
        assert_eq!(input.complete().1, 4);
        assert_eq!(input.line(), b"e");
        input.feed(b'\n');
        feed(&mut input, b"unknown");
        assert_eq!(input.complete().1, 0);
        input.feed(b'\n');
        assert_eq!(input.feed(b'\t'), Event::Complete);
        assert_eq!(input.complete().1, COMMANDS.len());
        assert!(input.line().is_empty());
    }

    #[test]
    fn crlf_cancel_eof_and_unsupported_escape_sequences() {
        let mut input = Input::new();
        feed(&mut input, b"echo x\x1b[99~\x1b[1;5D");
        assert_eq!(input.line(), b"echo x");
        assert_eq!(input.feed(b'\r'), Event::Submit);
        assert_eq!(input.feed(b'\n'), Event::None);
        feed(&mut input, b"x\x1b[");
        assert_eq!(input.feed(3), Event::Cancel);
        assert_eq!(input.feed(4), Event::Exit);
        feed(&mut input, b"x\x01");
        assert_eq!(input.feed(4), Event::Refresh);
        assert!(input.line().is_empty());
        assert_eq!(input.feed(4), Event::Exit);
    }

    #[test]
    fn long_lines_have_a_bounded_view_and_can_be_edited_at_capacity() {
        let mut input = Input::new();
        feed(&mut input, &[b'x'; LINE_MAX]);
        assert_eq!(input.view(), (&[b'x'; VIEW_WIDTH][..], 0));
        feed(&mut input, b"\x01\x04y");
        assert_eq!(input.line().len(), LINE_MAX);
        assert_eq!(input.line()[0], b'y');
        assert_eq!(input.view().1, VIEW_WIDTH - 1);
        assert_eq!(input.feed(b'\n'), Event::Submit);
    }
}
