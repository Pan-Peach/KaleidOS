//! 有界 whitespace tokenizer；不解释引号、管道、变量或程序执行。

use kcomp_sdk::management::ExecutionDomain;

pub const LINE_MAX: usize = 128;
const WORD_MAX: usize = 17;

#[derive(Debug, PartialEq, Eq)]
pub enum Command<'a> {
    Help,
    Echo(&'a [u8]),
    Clear,
    Components,
    Endpoints,
    Devices,
    Load(&'a [u8], ExecutionDomain),
    Inspect(&'a [u8]),
    Cat(&'a [u8]),
    Exit,
    Unsupported(&'a [u8]),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError<'a> {
    TooLong,
    TooManyWords,
    InvalidInput,
    InvalidArtifact,
    Usage(&'static str),
    UnsupportedDomain,
    Unknown(&'a [u8]),
}

pub fn domain(token: &[u8]) -> Option<ExecutionDomain> {
    match token {
        b"native" => Some(ExecutionDomain::KernelNative),
        b"isolated" => Some(ExecutionDomain::IsolatedNative),
        _ => None,
    }
}

fn artifact(token: &[u8]) -> Result<&[u8], ParseError<'_>> {
    let name = token.strip_suffix(b".kcomp").unwrap_or(token);
    if name.is_empty() || name.contains(&b'/') || name.contains(&b'\\') {
        Err(ParseError::InvalidArtifact)
    } else {
        Ok(name)
    }
}

pub fn parse(line: &[u8]) -> Result<Option<Command<'_>>, ParseError<'_>> {
    if line.len() > LINE_MAX {
        return Err(ParseError::TooLong);
    }
    if line
        .iter()
        .any(|b| !b.is_ascii_graphic() && !b.is_ascii_whitespace())
    {
        return Err(ParseError::InvalidInput);
    }
    let mut words = [b"".as_slice(); WORD_MAX];
    let mut len = 0;
    for token in line
        .split(|b| b.is_ascii_whitespace())
        .filter(|s| !s.is_empty())
    {
        if len == WORD_MAX {
            return Err(ParseError::TooManyWords);
        }
        words[len] = token;
        len += 1;
    }
    if len == 0 {
        return Ok(None);
    }
    let args = &words[1..len];
    let command = match words[0] {
        b"echo" => Command::Echo(line.trim_ascii()[words[0].len()..].trim_ascii()),
        b"load" => {
            if !(1..=2).contains(&args.len()) {
                return Err(ParseError::Usage("load <artifact> [native|isolated]"));
            }
            let domain = if args.len() == 1 {
                ExecutionDomain::KernelNative
            } else {
                domain(args[1]).ok_or(ParseError::UnsupportedDomain)?
            };
            Command::Load(artifact(args[0])?, domain)
        }
        b"inspect" => {
            if args.len() != 1 {
                return Err(ParseError::Usage("inspect <loaded-artifact>"));
            }
            Command::Inspect(artifact(args[0])?)
        }
        b"cat" => {
            if args.len() != 1 {
                return Err(ParseError::Usage("cat <provider-relative-path>"));
            }
            Command::Cat(args[0])
        }
        name @ (b"ls" | b"cd" | b"pwd") => Command::Unsupported(name),
        name => {
            let command = match name {
                b"help" => Command::Help,
                b"clear" => Command::Clear,
                b"components" => Command::Components,
                b"endpoints" => Command::Endpoints,
                b"devices" => Command::Devices,
                b"exit" => Command::Exit,
                _ => return Err(ParseError::Unknown(name)),
            };
            if !args.is_empty() {
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
    fn empty_and_whitespace_lines() {
        for line in [b"".as_slice(), b" \t\r\n "] {
            assert_eq!(parse(line), Ok(None));
        }
    }

    #[test]
    fn tokenization_preserves_words_and_tolerates_whitespace() {
        let Some(Command::Echo(words)) = parse(b" \techo   hello\tcore_test  ").unwrap() else {
            panic!()
        };
        assert_eq!(words, b"hello\tcore_test");
        let Some(Command::Echo(words)) = parse(b"echo").unwrap() else {
            panic!()
        };
        assert!(words.is_empty());
    }

    #[test]
    fn command_recognition() {
        for (line, expected) in [
            (b"help".as_slice(), Command::Help),
            (b"clear", Command::Clear),
            (b"components", Command::Components),
            (b"endpoints", Command::Endpoints),
            (b"devices", Command::Devices),
            (b"exit", Command::Exit),
        ] {
            assert_eq!(parse(line), Ok(Some(expected)));
        }
        assert_eq!(
            parse(b"cat HELLO.TXT"),
            Ok(Some(Command::Cat(b"HELLO.TXT")))
        );
        assert_eq!(
            parse(b"inspect virtio_blk.kcomp"),
            Ok(Some(Command::Inspect(b"virtio_blk")))
        );
        assert_eq!(parse(b"ls /"), Ok(Some(Command::Unsupported(b"ls"))));
    }

    #[test]
    fn malformed_arguments_and_unknown_commands() {
        for line in [
            b"load".as_slice(),
            b"load a native extra",
            b"inspect",
            b"cat a b",
            b"exit extra",
        ] {
            assert!(matches!(parse(line), Err(ParseError::Usage(_))));
        }
        assert_eq!(parse(b"unknown x"), Err(ParseError::Unknown(b"unknown")));
        assert_eq!(parse(b"./hello"), Err(ParseError::Unknown(b"./hello")));
        assert_eq!(parse(b"load /hello.elf"), Err(ParseError::InvalidArtifact));
        assert_eq!(parse(b"load .kcomp"), Err(ParseError::InvalidArtifact));
    }

    #[test]
    fn domains_are_requests_with_no_sandbox_fallback() {
        assert_eq!(
            parse(b"load x"),
            Ok(Some(Command::Load(b"x", ExecutionDomain::KernelNative)))
        );
        assert_eq!(
            parse(b"load x.kcomp isolated"),
            Ok(Some(Command::Load(b"x", ExecutionDomain::IsolatedNative)))
        );
        for token in [b"sandboxed".as_slice(), b"wasm", b"Native"] {
            assert_eq!(domain(token), None);
        }
        assert_eq!(
            parse(b"load x sandboxed"),
            Err(ParseError::UnsupportedDomain)
        );
    }

    #[test]
    fn excessive_and_invalid_input_is_rejected_before_dispatch() {
        assert_eq!(parse(&[b'x'; LINE_MAX + 1]), Err(ParseError::TooLong));
        assert_eq!(
            parse(b"echo a a a a a a a a a a a a a a a a a"),
            Err(ParseError::TooManyWords)
        );
        assert_eq!(parse(b"echo \0bad"), Err(ParseError::InvalidInput));
    }
}
