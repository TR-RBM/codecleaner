//! TypeScript support for codecleanup.
//!
//! TypeScript needs code rather than TOML rules for two reasons: template
//! literals nest (`${ .. }` holds code, which may hold another template),
//! and a `/` starts a regular expression or is a division depending on
//! what precedes it. Getting either wrong would turn the `//` of a URL in
//! a string into a comment.
//!
//! `.tsx` is left out on purpose: text between JSX tags is not quoted.

#![no_std]

use codecleanup_plugin::{
    export_plugin, Language, Segments, BLOCK_OPEN, CLOSE, CODE, LINE_OPEN, TEXT,
};

/// Bytes left unread at the end of a window, so that every decision can
/// look a few bytes ahead and sees keywords in one piece.
const MARGIN: usize = 64;
/// How deep template literals may nest before the rest is taken as text.
const MAX_NESTING: usize = 32;

#[derive(Clone, Copy)]
enum Mode {
    Code,
    /// A `#!` line at the start of the file.
    SkipLine,
    LineComment,
    BlockComment,
    Quoted(u8),
    Template,
    Regex {
        in_class: bool,
    },
}

struct TypeScript {
    mode: Mode,
    started: bool,
    /// The last token was a value, so a `/` divides it rather than
    /// starting a regular expression.
    after_value: bool,
    /// Open `{` per template substitution that is being read.
    braces: [u32; MAX_NESTING],
    nesting: usize,
}

export_plugin!(TypeScript);

impl Language for TypeScript {
    const INFO: &'static str = "name = \"TypeScript\"\nextensions = [\"ts\", \"mts\", \"cts\"]\n";
    const START: Self = TypeScript {
        mode: Mode::Code,
        started: false,
        after_value: false,
        braces: [0; MAX_NESTING],
        nesting: 0,
    };

    fn scan(&mut self, input: &[u8], eof: bool, out: &mut Segments) -> usize {
        let len = input.len();
        if !self.started {
            if len < 2 && !eof {
                return 0;
            }
            self.started = true;
            if input.starts_with(b"#!") {
                self.mode = Mode::SkipLine;
            }
        }
        let stop = if eof { len } else { len.saturating_sub(MARGIN) };
        // `start..i` is a run of code, or of text inside a comment.
        let mut start = 0;
        let mut i = 0;
        while i < stop && out.has_room() {
            let byte = input[i];
            let next = input.get(i + 1).copied();
            match self.mode {
                Mode::Code => match byte {
                    b'/' if next == Some(b'/') => {
                        out.push(CODE, i - start);
                        out.push(LINE_OPEN, 2);
                        self.mode = Mode::LineComment;
                        i += 2;
                        start = i;
                    }
                    b'/' if next == Some(b'*') => {
                        // `/**` opens a doc comment, but `/**/` is empty.
                        let doc =
                            input.get(i + 2) == Some(&b'*') && input.get(i + 3) != Some(&b'/');
                        out.push(CODE, i - start);
                        out.push(BLOCK_OPEN, 2 + doc as usize);
                        self.mode = Mode::BlockComment;
                        i += 2 + doc as usize;
                        start = i;
                    }
                    b'/' if !self.after_value => {
                        self.mode = Mode::Regex { in_class: false };
                        i += 1;
                    }
                    b'"' | b'\'' => {
                        self.mode = Mode::Quoted(byte);
                        i += 1;
                    }
                    b'`' => {
                        self.mode = Mode::Template;
                        i += 1;
                    }
                    b'{' => {
                        if self.nesting > 0 {
                            self.braces[self.nesting - 1] += 1;
                        }
                        self.after_value = false;
                        i += 1;
                    }
                    b'}' => {
                        if self.nesting > 0 && self.braces[self.nesting - 1] == 0 {
                            // The end of a `${ .. }`: back to the template.
                            self.nesting -= 1;
                            self.mode = Mode::Template;
                        } else if self.nesting > 0 {
                            self.braces[self.nesting - 1] -= 1;
                        }
                        self.after_value = false;
                        i += 1;
                    }
                    b')' | b']' => {
                        self.after_value = true;
                        i += 1;
                    }
                    b' ' | b'\t' | b'\r' | b'\n' => i += 1,
                    _ if is_word(byte) => {
                        let end = i + input[i..].iter().take_while(|&&b| is_word(b)).count();
                        self.after_value = !takes_operand(&input[i..end]);
                        i = end;
                    }
                    _ => {
                        self.after_value = false;
                        i += 1;
                    }
                },
                Mode::SkipLine => {
                    if byte == b'\n' {
                        self.mode = Mode::Code;
                    }
                    i += 1;
                }
                Mode::LineComment => {
                    if byte == b'\n' {
                        out.push(TEXT, i - start);
                        self.mode = Mode::Code;
                        start = i;
                    } else {
                        i += 1;
                    }
                }
                Mode::BlockComment => {
                    if byte == b'*' && next == Some(b'/') {
                        out.push(TEXT, i - start);
                        out.push(CLOSE, 2);
                        self.mode = Mode::Code;
                        i += 2;
                        start = i;
                    } else {
                        i += 1;
                    }
                }
                Mode::Quoted(quote) => match byte {
                    b'\\' => i += 2,
                    // An unterminated string ends with its line.
                    b'\n' => self.mode = Mode::Code,
                    _ => {
                        if byte == quote {
                            self.mode = Mode::Code;
                            self.after_value = true;
                        }
                        i += 1;
                    }
                },
                Mode::Template => match byte {
                    b'\\' => i += 2,
                    b'`' => {
                        self.mode = Mode::Code;
                        self.after_value = true;
                        i += 1;
                    }
                    b'$' if next == Some(b'{') && self.nesting < MAX_NESTING => {
                        self.braces[self.nesting] = 0;
                        self.nesting += 1;
                        self.mode = Mode::Code;
                        self.after_value = false;
                        i += 2;
                    }
                    _ => i += 1,
                },
                Mode::Regex { in_class } => match byte {
                    b'\\' => i += 2,
                    // Not a regular expression after all.
                    b'\n' => self.mode = Mode::Code,
                    b'/' if !in_class => {
                        self.mode = Mode::Code;
                        self.after_value = true;
                        i += 1;
                    }
                    _ => {
                        if byte == b'[' || byte == b']' {
                            self.mode = Mode::Regex {
                                in_class: byte == b'[',
                            };
                        }
                        i += 1;
                    }
                },
            }
        }
        let i = i.min(len);
        let in_comment = matches!(self.mode, Mode::LineComment | Mode::BlockComment);
        out.push(if in_comment { TEXT } else { CODE }, i - start);
        i
    }
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

/// Whether a `/` after this word starts a regular expression: it does
/// after a keyword that takes an operand, not after a name or a number.
fn takes_operand(word: &[u8]) -> bool {
    const KEYWORDS: [&[u8]; 14] = [
        b"return",
        b"typeof",
        b"instanceof",
        b"in",
        b"of",
        b"new",
        b"delete",
        b"void",
        b"throw",
        b"case",
        b"do",
        b"else",
        b"yield",
        b"await",
    ];
    KEYWORDS.contains(&word)
}
