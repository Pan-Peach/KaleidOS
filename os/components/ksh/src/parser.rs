//! 有界参数解析；引号和转义由 shell 解释，路径语义仍交给 provider。
use kcomp_sdk::management::ExecutionDomain;

pub const LINE_MAX: usize = 512;
const WORD_MAX: usize = 17;

// Help and command-name completion share an inventory, not a dispatch registry.
pub const COMMANDS: &[(&[u8], &str)] = &[
    (b"help", "help [command]"),
    (b"echo", "echo [args...]"),
    (b"clear", "clear"),
    (b"history", "history"),
    (b"components", "components"),
    (b"endpoints", "endpoints"),
    (b"devices", "devices"),
    (b"load", "load <artifact> [native|isolated]"),
    (b"inspect", "inspect <loaded-artifact>"),
    (b"cat", "cat <provider-relative-path>"),
    (b"exec", "exec <provider-relative-path> [args...]"),
    (b"exit", "exit"),
    (b"ls", "ls: unavailable (no directory service)"),
    (b"cd", "cd: unavailable (no namespace service)"),
    (b"pwd", "pwd: unavailable (no namespace service)"),
];

#[derive(Debug, PartialEq, Eq)]
pub enum Command<'a> {
    Help(Option<&'a [u8]>),
    Echo(Args<'a>),
    Clear,
    History,
    Components,
    Endpoints,
    Devices,
    Load(&'a [u8], ExecutionDomain),
    Inspect(&'a [u8]),
    Cat(&'a [u8]),
    Exec(Args<'a>),
    Exit,
    Unsupported(&'a [u8]),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError<'a> {
    TooLong,
    TooManyWords,
    InvalidInput,
    InvalidArtifact,
    UnclosedQuote,
    TrailingEscape,
    UnsupportedSyntax,
    Usage(&'static str),
    UnsupportedDomain,
    Unknown(&'a [u8]),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Args<'a> {
    bytes: &'a [u8],
    ranges: [(u16, u16); WORD_MAX],
    len: usize,
}

impl<'a> Args<'a> {
    fn get(&self, index: usize) -> &'a [u8] {
        let (start, end) = self.ranges[index];
        &self.bytes[usize::from(start)..usize::from(end)]
    }

    pub fn iter(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.ranges[..self.len]
            .iter()
            .map(|&(start, end)| &self.bytes[usize::from(start)..usize::from(end)])
    }
}

pub fn domain(token: &[u8]) -> Option<ExecutionDomain> {
    match token {
        b"native" => Some(ExecutionDomain::KernelNative),
        b"isolated" => Some(ExecutionDomain::IsolatedNative),
        _ => None,
    }
}

fn artifact(token: &[u8]) -> Result<&[u8], ParseError<'_>> {
    let token = token.strip_suffix(b".kcomp").unwrap_or(token);
    if token.is_empty() || token.contains(&b'/') || token.contains(&b'\\') {
        Err(ParseError::InvalidArtifact)
    } else {
        Ok(token)
    }
}

fn words<'a>(line: &[u8], buffer: &'a mut [u8; LINE_MAX]) -> Result<Args<'a>, ParseError<'a>> {
    if line.len() > LINE_MAX {
        return Err(ParseError::TooLong);
    }
    if line
        .iter()
        .any(|b| !b.is_ascii_graphic() && *b != b' ' && *b != b'\t')
    {
        return Err(ParseError::InvalidInput);
    }
    let mut ranges = [(0, 0); WORD_MAX];
    let mut count = 0;
    let mut end = 0;
    let mut start = 0;
    let mut started = false;
    let mut quote = None;
    let mut bytes = line.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == b'\\' && quote != Some(b'\'') {
            let next = bytes.next().ok_or(ParseError::TrailingEscape)?;
            // In double quotes, only quote and backslash consume an escape.
            if quote == Some(b'"') && next != b'"' && next != b'\\' {
                buffer[end] = b'\\';
                end += 1;
            }
            buffer[end] = next;
            end += 1;
            started = true;
        } else if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
            } else {
                buffer[end] = byte;
                end += 1;
            }
        } else {
            match byte {
                b'\'' | b'"' => {
                    quote = Some(byte);
                    started = true;
                }
                b' ' | b'\t' => {
                    if started {
                        ranges[count] = (start as u16, end as u16);
                        count += 1;
                        start = end;
                        started = false;
                    }
                }
                b'#' if !started => break,
                // Never execute a prefix of a pipeline, redirect, or command list.
                b'|' | b'&' | b';' | b'<' | b'>' => return Err(ParseError::UnsupportedSyntax),
                _ => {
                    buffer[end] = byte;
                    end += 1;
                    started = true;
                }
            }
        }
        if count == WORD_MAX && started {
            return Err(ParseError::TooManyWords);
        }
    }
    if quote.is_some() {
        return Err(ParseError::UnclosedQuote);
    }
    if started {
        ranges[count] = (start as u16, end as u16);
        count += 1;
    }
    Ok(Args {
        bytes: &buffer[..end],
        ranges,
        len: count,
    })
}

pub fn parse<'a>(
    line: &[u8],
    buffer: &'a mut [u8; LINE_MAX],
) -> Result<Option<Command<'a>>, ParseError<'a>> {
    let mut args = words(line, buffer)?;
    if args.len == 0 {
        return Ok(None);
    }
    let name = args.get(0);
    args.ranges.copy_within(1..args.len, 0);
    args.len -= 1;
    let command = match name {
        b"echo" => Command::Echo(args),
        b"help" => {
            if args.len > 1 {
                return Err(ParseError::Usage("help [command]"));
            }
            Command::Help(if args.len == 0 {
                None
            } else {
                Some(args.get(0))
            })
        }
        b"load" => {
            if !(1..=2).contains(&args.len) {
                return Err(ParseError::Usage("load <artifact> [native|isolated]"));
            }
            let domain = if args.len == 1 {
                ExecutionDomain::KernelNative
            } else {
                domain(args.get(1)).ok_or(ParseError::UnsupportedDomain)?
            };
            Command::Load(artifact(args.get(0))?, domain)
        }
        b"inspect" => {
            if args.len != 1 {
                return Err(ParseError::Usage("inspect <loaded-artifact>"));
            }
            Command::Inspect(artifact(args.get(0))?)
        }
        b"exec" => {
            if args.len == 0 || args.get(0).is_empty() {
                return Err(ParseError::Usage("exec <provider-relative-path> [args...]"));
            }
            Command::Exec(args)
        }
        b"cat" => {
            if args.len != 1 || args.get(0).is_empty() {
                return Err(ParseError::Usage("cat <provider-relative-path>"));
            }
            Command::Cat(args.get(0))
        }
        b"ls" | b"cd" | b"pwd" => Command::Unsupported(name),
        _ => {
            let command = match name {
                b"clear" => Command::Clear,
                b"history" => Command::History,
                b"components" => Command::Components,
                b"endpoints" => Command::Endpoints,
                b"devices" => Command::Devices,
                b"exit" => Command::Exit,
                _ => return Err(ParseError::Unknown(name)),
            };
            if args.len != 0 {
                return Err(ParseError::Usage("this command takes no arguments"));
            }
            command
        }
    };
    Ok(Some(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_escapes_empty_arguments_and_comments() {
        let mut buffer = [0; LINE_MAX];
        let args = words(
            br#" echo "hello  world" 'a\b' c\ d '' pre"fix" # ignored"#,
            &mut buffer,
        )
        .unwrap();
        assert_eq!(
            args.iter().collect::<std::vec::Vec<_>>(),
            [
                b"echo".as_slice(),
                b"hello  world",
                b"a\\b",
                b"c d",
                b"",
                b"prefix"
            ]
        );
        let args = words(br#"echo "a\"b\\c\d" x#y \#z"#, &mut buffer).unwrap();
        assert_eq!(
            args.iter().collect::<std::vec::Vec<_>>(),
            [b"echo".as_slice(), b"a\"b\\c\\d", b"x#y", b"#z"]
        );
    }

    #[test]
    fn empty_and_comment_lines() {
        let mut buffer = [0; LINE_MAX];
        for line in [b"".as_slice(), b" \t ", b"# comment", b"  # comment"] {
            assert_eq!(parse(line, &mut buffer), Ok(None));
        }
    }

    #[test]
    fn command_inventory_and_quoted_paths() {
        let mut buffer = [0; LINE_MAX];
        for &(name, _) in COMMANDS {
            assert!(!matches!(
                parse(name, &mut buffer),
                Err(ParseError::Unknown(_))
            ));
        }
        assert_eq!(
            parse(b"help cat", &mut buffer),
            Ok(Some(Command::Help(Some(b"cat"))))
        );
        assert_eq!(
            parse(b"cat '0:/HELLO WORLD.TXT'", &mut buffer),
            Ok(Some(Command::Cat(b"0:/HELLO WORLD.TXT")))
        );
        assert_eq!(
            parse(b"inspect virtio_blk.kcomp", &mut buffer),
            Ok(Some(Command::Inspect(b"virtio_blk")))
        );
        let Some(Command::Exec(args)) = parse(b"exec APP.ELF 'two words' ''", &mut buffer).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            args.iter().collect::<std::vec::Vec<_>>(),
            [b"APP.ELF".as_slice(), b"two words", b""]
        );
        let Some(Command::Echo(args)) = parse(b"echo", &mut buffer).unwrap() else {
            panic!()
        };
        assert!(args.iter().collect::<std::vec::Vec<_>>().is_empty());
        assert_eq!(
            parse(b"ls /", &mut buffer),
            Ok(Some(Command::Unsupported(b"ls")))
        );
    }

    #[test]
    fn malformed_input_is_rejected_before_dispatch() {
        let mut buffer = [0; LINE_MAX];
        for line in [
            b"load".as_slice(),
            b"exec",
            b"exec ''",
            b"load a native extra",
            b"inspect",
            b"cat a b",
            b"cat ''",
            b"exit extra",
            b"help a b",
        ] {
            assert!(matches!(
                parse(line, &mut buffer),
                Err(ParseError::Usage(_))
            ));
        }
        for line in [b"echo 'bad".as_slice(), b"load x \"native"] {
            assert_eq!(parse(line, &mut buffer), Err(ParseError::UnclosedQuote));
        }
        assert_eq!(
            parse(b"echo bad\\", &mut buffer),
            Err(ParseError::TrailingEscape)
        );
        for line in [
            b"load x; exit".as_slice(),
            b"cat x | cat y",
            b"echo x > y",
            b"exit &",
            b"cat < x",
        ] {
            assert_eq!(parse(line, &mut buffer), Err(ParseError::UnsupportedSyntax));
        }
        assert!(parse(br#"echo "|;<>&" \|"#, &mut buffer).is_ok());
        assert_eq!(
            parse(b"unknown x", &mut buffer),
            Err(ParseError::Unknown(b"unknown"))
        );
        assert_eq!(
            parse(b"load /hello.elf", &mut buffer),
            Err(ParseError::InvalidArtifact)
        );
        assert_eq!(
            parse(b"load .kcomp", &mut buffer),
            Err(ParseError::InvalidArtifact)
        );
    }

    #[test]
    fn domains_are_requests_with_no_sandbox_fallback() {
        let mut buffer = [0; LINE_MAX];
        assert_eq!(
            parse(b"load x", &mut buffer),
            Ok(Some(Command::Load(b"x", ExecutionDomain::KernelNative)))
        );
        assert_eq!(
            parse(b"load x.kcomp isolated", &mut buffer),
            Ok(Some(Command::Load(b"x", ExecutionDomain::IsolatedNative)))
        );
        assert_eq!(
            parse(b"load x sandboxed", &mut buffer),
            Err(ParseError::UnsupportedDomain)
        );
        for token in [b"sandboxed".as_slice(), b"wasm", b"Native"] {
            assert_eq!(domain(token), None);
        }
    }

    #[test]
    fn byte_and_word_limits_include_empty_arguments() {
        let mut buffer = [0; LINE_MAX];
        assert_eq!(
            parse(&[b'x'; LINE_MAX + 1], &mut buffer),
            Err(ParseError::TooLong)
        );
        assert!(words(&[b'x'; LINE_MAX], &mut buffer).is_ok());
        assert!(parse(b"echo a a a a a a a a a a a a a a a a", &mut buffer).is_ok());
        assert_eq!(
            parse(b"echo a a a a a a a a a a a a a a a a a", &mut buffer),
            Err(ParseError::TooManyWords)
        );
        assert_eq!(
            parse(
                b"echo '' '' '' '' '' '' '' '' '' '' '' '' '' '' '' '' ''",
                &mut buffer
            ),
            Err(ParseError::TooManyWords)
        );
        for line in [
            b"echo \0bad".as_slice(),
            b"echo \x1bbad",
            b"echo\nbad",
            b"echo \xff",
        ] {
            assert_eq!(parse(line, &mut buffer), Err(ParseError::InvalidInput));
        }
    }
}
