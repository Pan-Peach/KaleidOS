//! 串口的一行输入。溢出后丢弃整行，绝不执行截断前缀。
use crate::parser::LINE_MAX;

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    None,
    Echo(u8),
    Erase,
    Submit,
    Overflow,
    Cancel,
    Exit,
}

pub struct Input {
    bytes: [u8; LINE_MAX],
    len: usize,
    overflow: bool,
    reset: bool,
    skip_lf: bool,
}

impl Input {
    pub fn new() -> Self {
        Self {
            bytes: [0; LINE_MAX],
            len: 0,
            overflow: false,
            reset: false,
            skip_lf: false,
        }
    }
    pub fn line(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
    pub fn feed(&mut self, byte: u8) -> Event {
        if self.reset {
            self.len = 0;
            self.overflow = false;
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
                if self.overflow {
                    Event::Overflow
                } else {
                    Event::Submit
                }
            }
            3 => {
                self.reset = true;
                Event::Cancel
            }
            4 if self.len == 0 && !self.overflow => Event::Exit,
            8 | 127 if self.len > 0 && !self.overflow => {
                self.len -= 1;
                Event::Erase
            }
            b'\t' | 32..=126 => {
                let byte = if byte == b'\t' { b' ' } else { byte };
                if self.len == LINE_MAX || self.overflow {
                    self.overflow = true;
                    return Event::None;
                }
                self.bytes[self.len] = byte;
                self.len += 1;
                Event::Echo(byte)
            }
            _ => Event::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overflow_discards_whole_line_and_next_command_recovers() {
        let mut input = Input::new();
        for _ in 0..=LINE_MAX {
            input.feed(b'x');
        }
        input.feed(127);
        assert_eq!(input.feed(b'\n'), Event::Overflow);
        for &b in b"help" {
            input.feed(b);
        }
        assert_eq!(input.feed(b'\n'), Event::Submit);
        assert_eq!(input.line(), b"help");
    }
    #[test]
    fn backspace_crlf_cancel_and_eof() {
        let mut input = Input::new();
        input.feed(b'a');
        input.feed(b'b');
        assert_eq!(input.feed(127), Event::Erase);
        assert_eq!(input.feed(b'\r'), Event::Submit);
        assert_eq!(input.line(), b"a");
        assert_eq!(input.feed(b'\n'), Event::None);
        input.feed(b'x');
        assert_eq!(input.feed(3), Event::Cancel);
        assert_eq!(input.feed(4), Event::Exit);
    }
}
