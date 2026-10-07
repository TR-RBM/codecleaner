//! Lua support for codecleanup.
//!
//! Lua needs code rather than TOML rules because of its long brackets:
//! `--[==[ .. ]==]` closes only at a bracket with the same number of `=`.

#![no_std]

use codecleanup_plugin::{
    export_plugin, Language, Segments, BLOCK_OPEN, CLOSE, CODE, LINE_OPEN, TEXT,
};

/// Bytes left unread at the end of a window so that a token is never cut
/// in half. Long brackets with more `=` than this are not recognised.
const MARGIN: usize = 64;

#[derive(Clone, Copy)]
enum State {
    Code,
    /// The first line if it starts with `#`, which Lua ignores.
    SkipLine,
    Quoted(u8),
    LongString(usize),
    LineComment,
    LongComment(usize),
}

struct Lua {
    state: State,
    started: bool,
}

export_plugin!(Lua);

impl Language for Lua {
    const INFO: &'static str = "name = \"Lua\"\nextensions = [\"lua\"]\n";
    const START: Self = Lua {
        state: State::Code,
        started: false,
    };

    fn scan(&mut self, input: &[u8], eof: bool, out: &mut Segments) -> usize {
        let len = input.len();
        if !self.started {
            if len == 0 && !eof {
                return 0;
            }
            self.started = true;
            if input.first() == Some(&b'#') {
                self.state = State::SkipLine;
            }
        }
        let limit = if eof { len } else { len.saturating_sub(MARGIN) };
        let mut pos = 0;
        // Every round pushes at most three segments.
        while pos < len && out.has_room() {
            match self.state {
                State::Code => {
                    let stop = limit.max(pos);
                    let mut i = pos;
                    while i < stop {
                        match input[i] {
                            b'-' if input.get(i + 1) == Some(&b'-') => {
                                out.push(CODE, i - pos);
                                if let Some(level) = long_bracket(&input[i + 2..]) {
                                    out.push(BLOCK_OPEN, level + 4);
                                    self.state = State::LongComment(level);
                                    pos = i + level + 4;
                                } else {
                                    out.push(LINE_OPEN, 2);
                                    self.state = State::LineComment;
                                    pos = i + 2;
                                }
                                break;
                            }
                            quote @ (b'"' | b'\'') => {
                                out.push(CODE, i + 1 - pos);
                                self.state = State::Quoted(quote);
                                pos = i + 1;
                                break;
                            }
                            b'[' => {
                                if let Some(level) = long_bracket(&input[i..]) {
                                    out.push(CODE, i + level + 2 - pos);
                                    self.state = State::LongString(level);
                                    pos = i + level + 2;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                    if i >= stop {
                        out.push(CODE, stop - pos);
                        return stop;
                    }
                }
                State::SkipLine | State::LineComment => {
                    let skip = matches!(self.state, State::SkipLine);
                    let kind = if skip { CODE } else { TEXT };
                    match input[pos..].iter().position(|&b| b == b'\n') {
                        // The newline is code; a skipped line takes it along.
                        Some(i) => {
                            out.push(kind, i + skip as usize);
                            self.state = State::Code;
                            pos += i + skip as usize;
                        }
                        None => {
                            out.push(kind, len - pos);
                            pos = len;
                        }
                    }
                }
                State::Quoted(quote) => {
                    let mut i = pos;
                    while i < len {
                        match input[i] {
                            b'\\' if i + 1 >= len && !eof => {
                                out.push(CODE, i - pos);
                                return i;
                            }
                            b'\\' => i += 1,
                            b'\n' => {
                                self.state = State::Code;
                                break;
                            }
                            b if b == quote => {
                                self.state = State::Code;
                                i += 1;
                                break;
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                    let end = i.min(len);
                    out.push(CODE, end - pos);
                    pos = end;
                }
                State::LongString(level) | State::LongComment(level) => {
                    let comment = matches!(self.state, State::LongComment(_));
                    let kind = if comment { TEXT } else { CODE };
                    let mut i = pos;
                    let mut closed = false;
                    while i < len {
                        if input[i] == b']' {
                            if i + level + 2 > len && !eof {
                                // The bracket may continue in the next window.
                                out.push(kind, i - pos);
                                return i;
                            }
                            if closes(&input[i..], level) {
                                closed = true;
                                break;
                            }
                        }
                        i += 1;
                    }
                    out.push(kind, i - pos);
                    pos = i;
                    if closed {
                        out.push(if comment { CLOSE } else { CODE }, level + 2);
                        self.state = State::Code;
                        pos += level + 2;
                    }
                }
            }
        }
        pos
    }
}

/// The level of the long bracket `[`, `=`.., `[` at the start of `input`.
fn long_bracket(input: &[u8]) -> Option<usize> {
    if input.first() != Some(&b'[') {
        return None;
    }
    let level = input[1..].iter().take_while(|&&b| b == b'=').count();
    (input.get(level + 1) == Some(&b'[')).then_some(level)
}

/// Whether `input` starts with the closing long bracket of `level`.
fn closes(input: &[u8], level: usize) -> bool {
    input.len() >= level + 2
        && input[1..level + 1].iter().all(|&b| b == b'=')
        && input[level + 1] == b']'
}
