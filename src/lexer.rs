//! The contract between the host and a language plugin.
//!
//! A plugin never touches files. The host hands it a window of bytes and the
//! plugin classifies them into [`Segment`]s; reading, writing and buffering
//! stay on the host side.

use anyhow::Result;

/// What a run of bytes is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Kind {
    /// Anything that is not a comment, string literals included.
    Code = 0,
    /// The token that starts a line comment, e.g. `//`.
    LineOpen = 1,
    /// The token that starts a block comment, e.g. `/*`.
    BlockOpen = 2,
    /// Comment text. A line comment ends before its newline.
    Text = 3,
    /// The token that ends a block comment, e.g. `*/`.
    Close = 4,
}

impl Kind {
    pub fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0 => Kind::Code,
            1 => Kind::LineOpen,
            2 => Kind::BlockOpen,
            3 => Kind::Text,
            4 => Kind::Close,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub kind: Kind,
    pub len: usize,
}

/// A streaming comment lexer for one language.
pub trait Lexer {
    /// Forgets all state so the next call starts a new file.
    fn reset(&mut self);

    /// Classifies a prefix of `input` and returns how many bytes it covers.
    ///
    /// The segments appended to `out` cover exactly the returned number of
    /// bytes, in order. Unconsumed bytes are handed in again at the start of
    /// the next call, together with more data. With `eof` set, `input` is
    /// the rest of the file and has to be consumed completely.
    fn scan(&mut self, input: &[u8], eof: bool, out: &mut Vec<Segment>) -> Result<usize>;
}

/// Appends a segment, merging it into the previous one if the kind matches.
pub fn push(out: &mut Vec<Segment>, kind: Kind, len: usize) {
    if len == 0 {
        return;
    }
    match out.last_mut() {
        Some(last) if last.kind == kind && matches!(kind, Kind::Code | Kind::Text) => {
            last.len += len
        }
        _ => out.push(Segment { kind, len }),
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;

    /// Runs a lexer over `src` in windows of at most `chunk` bytes and
    /// returns every segment with its text.
    pub fn lex(lexer: &mut dyn Lexer, src: &[u8], chunk: usize) -> Vec<(Kind, String)> {
        lexer.reset();
        let mut result: Vec<(Kind, String)> = Vec::new();
        let (mut start, mut end) = (0, chunk.min(src.len()));
        loop {
            let eof = end == src.len();
            let mut segments = Vec::new();
            let used = lexer.scan(&src[start..end], eof, &mut segments).unwrap();
            let mut at = start;
            for segment in segments {
                let text = String::from_utf8_lossy(&src[at..at + segment.len]).into_owned();
                match result.last_mut() {
                    Some((kind, last))
                        if *kind == segment.kind
                            && matches!(segment.kind, Kind::Code | Kind::Text) =>
                    {
                        last.push_str(&text)
                    }
                    _ => result.push((segment.kind, text)),
                }
                at += segment.len;
            }
            assert_eq!(at, start + used, "segments must cover the consumed bytes");
            start += used;
            if eof && start == src.len() {
                return result;
            }
            assert!(used > 0 || !eof, "lexer stalled at end of file");
            end = (end + chunk).min(src.len());
        }
    }

    /// The comment-related segments only, as `(kind, text)`.
    pub fn comments(lexer: &mut dyn Lexer, src: &str, chunk: usize) -> Vec<(Kind, String)> {
        lex(lexer, src.as_bytes(), chunk)
            .into_iter()
            .filter(|(kind, _)| *kind != Kind::Code)
            .collect()
    }
}
