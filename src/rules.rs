//! Declarative language plugins: a TOML file names the comment and string
//! tokens of a language and [`RuleLexer`] scans for them natively.

use std::sync::Arc;

use aho_corasick::{AhoCorasick, Input, MatchKind};
use anyhow::{Result, bail, ensure};
use memchr::memmem::Finder;
use serde::Deserialize;

use crate::lexer::{Kind, Lexer, Segment, push};

/// A language definition as written in a `.toml` plugin.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub name: String,
    /// File extensions without the dot, matched case-insensitively.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Exact file names, for files without a telling extension.
    #[serde(default)]
    pub filenames: Vec<String>,
    #[serde(default)]
    line_comments: Vec<String>,
    /// A line comment only starts at the beginning of a line or after
    /// whitespace. For languages where the token also has other uses, like
    /// `$#` in a Makefile recipe.
    #[serde(default)]
    line_comments_need_space: bool,
    #[serde(default)]
    block_comments: Vec<BlockDef>,
    #[serde(default)]
    strings: Vec<StringDef>,
    /// Comments whose text starts with one of these are left untouched,
    /// e.g. linter directives.
    #[serde(default)]
    keep: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockDef {
    open: String,
    close: String,
    #[serde(default)]
    nested: bool,
    /// Single characters that belong to the opening token when they follow
    /// it directly, e.g. `*` so that `/** .. */` stays a doc comment.
    #[serde(default)]
    doc_markers: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StringDef {
    open: String,
    /// Defaults to `open`.
    close: Option<String>,
    escape: Option<String>,
    #[serde(default = "yes")]
    multiline: bool,
    /// A character literal: only a literal if it closes after one character
    /// or one escape sequence, which tells `'a'` from a Rust lifetime `'a`.
    #[serde(default)]
    char: bool,
    /// A character that may repeat between `open` and `close` and then has
    /// to follow the closing `close` as often: with `open = "r"`,
    /// `close = "\""` and `fence = "#"` this is a Rust raw string,
    /// `r"..."`, `r#"..."#`, `r##"..."##` and so on.
    fence: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy)]
enum Action {
    Line,
    Block(usize),
    Str(usize),
}

#[derive(Debug)]
struct Block {
    open: Finder<'static>,
    close: Finder<'static>,
    nested: bool,
    markers: Vec<u8>,
}

#[derive(Debug)]
struct Str {
    open_len: usize,
    close: Vec<u8>,
    escape: Option<u8>,
    multiline: bool,
    char: bool,
    fence: Option<u8>,
}

/// A compiled [`Definition`].
#[derive(Debug)]
pub struct Rules {
    tokens: AhoCorasick,
    actions: Vec<Action>,
    blocks: Vec<Block>,
    strings: Vec<Str>,
    keep: Vec<Vec<u8>>,
    line_needs_space: bool,
    /// Bytes this close to the end of a window may be a cut-off token.
    margin: usize,
}

/// How far a character literal can reach: `'\u{10FFFF}'`.
const MAX_CHAR_LITERAL: usize = 12;
/// The most fence characters a fenced string may use.
const MAX_FENCES: usize = 32;
/// How much whitespace may sit between a comment token and a `keep` prefix.
const MAX_KEEP_GAP: usize = 8;

impl Rules {
    pub fn compile(def: &Definition) -> Result<Self> {
        ensure!(
            !def.extensions.is_empty() || !def.filenames.is_empty(),
            "needs at least one entry in `extensions` or `filenames`"
        );
        let mut patterns: Vec<String> = Vec::new();
        let mut actions = Vec::new();
        let mut longest = 0;
        let mut measure = |token: &str| -> Result<()> {
            ensure!(!token.is_empty(), "tokens must not be empty");
            longest = longest.max(token.len());
            Ok(())
        };
        for token in &def.line_comments {
            measure(token)?;
            patterns.push(token.clone());
            actions.push(Action::Line);
        }
        let mut blocks = Vec::new();
        for (i, block) in def.block_comments.iter().enumerate() {
            measure(&block.open)?;
            measure(&block.close)?;
            patterns.push(block.open.clone());
            actions.push(Action::Block(i));
            let mut markers = Vec::new();
            for marker in &block.doc_markers {
                ensure!(
                    marker.len() == 1,
                    "doc markers must be single ASCII characters"
                );
                markers.push(marker.as_bytes()[0]);
            }
            blocks.push(Block {
                open: Finder::new(&block.open).into_owned(),
                close: Finder::new(&block.close).into_owned(),
                nested: block.nested,
                markers,
            });
        }
        let mut strings = Vec::new();
        for (i, string) in def.strings.iter().enumerate() {
            let close = string.close.as_deref().unwrap_or(&string.open);
            measure(&string.open)?;
            measure(close)?;
            let single_byte = |option: &Option<String>, name| match option.as_deref() {
                None => Ok(None),
                Some(text) if text.len() == 1 => Ok(Some(text.as_bytes()[0])),
                Some(_) => bail!("`{name}` must be a single ASCII character"),
            };
            let fence = single_byte(&string.fence, "fence")?;
            match &string.fence {
                // The token search finds the start of either form.
                Some(fence) => {
                    for next in [close, fence] {
                        patterns.push(format!("{}{next}", string.open));
                        actions.push(Action::Str(i));
                    }
                }
                None => {
                    patterns.push(string.open.clone());
                    actions.push(Action::Str(i));
                }
            }
            strings.push(Str {
                open_len: string.open.len(),
                close: close.as_bytes().to_vec(),
                escape: single_byte(&string.escape, "escape")?,
                multiline: string.multiline,
                char: string.char,
                fence,
            });
        }
        for (i, pattern) in patterns.iter().enumerate() {
            ensure!(
                !patterns[..i].contains(pattern),
                "token `{pattern}` is defined twice"
            );
        }
        let keep: Vec<Vec<u8>> = def.keep.iter().map(|k| k.as_bytes().to_vec()).collect();
        let keep_len = keep.iter().map(Vec::len).max().unwrap_or(0);
        let tokens = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(&patterns)?;
        Ok(Rules {
            tokens,
            actions,
            blocks,
            strings,
            keep,
            line_needs_space: def.line_comments_need_space,
            margin: longest
                + (MAX_KEEP_GAP + keep_len)
                    .max(MAX_CHAR_LITERAL)
                    .max(MAX_FENCES + 1),
        })
    }

    /// Whether the comment text at the start of `rest` is on the keep list.
    fn kept(&self, rest: &[u8]) -> bool {
        let gap = rest
            .iter()
            .take(MAX_KEEP_GAP)
            .take_while(|b| matches!(b, b' ' | b'\t'))
            .count();
        self.keep.iter().any(|k| rest[gap..].starts_with(k))
    }
}

#[derive(Debug, Clone, Copy)]
enum State {
    Code,
    /// Code up to and including the next newline, without looking at it.
    SkipLine,
    Line,
    Block {
        idx: usize,
        depth: u32,
    },
    Str {
        idx: usize,
        fences: usize,
    },
}

pub struct RuleLexer {
    rules: Arc<Rules>,
    state: State,
    /// The last byte consumed so far.
    prev: u8,
    started: bool,
}

impl RuleLexer {
    pub fn new(rules: Arc<Rules>) -> Self {
        RuleLexer {
            rules,
            state: State::Code,
            prev: b'\n',
            started: false,
        }
    }
}

impl Lexer for RuleLexer {
    fn reset(&mut self) {
        self.state = State::Code;
        self.prev = b'\n';
        self.started = false;
    }

    fn scan(&mut self, input: &[u8], eof: bool, out: &mut Vec<Segment>) -> Result<usize> {
        let rules = Arc::clone(&self.rules);
        let len = input.len();
        if !self.started {
            if len < 3 && !eof {
                return Ok(0);
            }
            self.started = true;
            // A shebang is not a comment; `#![` is a Rust attribute.
            if input.starts_with(b"#!") && input.get(2) != Some(&b'[') {
                self.state = State::SkipLine;
            }
        }
        let limit = if eof {
            len
        } else {
            len.saturating_sub(rules.margin)
        };
        let mut pos = 0;
        while pos < len {
            match self.state {
                State::Code => {
                    let mut from = pos;
                    let found = loop {
                        let hit = rules.tokens.find(Input::new(input).range(from..));
                        let Some(hit) = hit.filter(|hit| hit.start() < limit) else {
                            break None;
                        };
                        let start = hit.start();
                        let before = if start == 0 {
                            self.prev
                        } else {
                            input[start - 1]
                        };
                        if before == b'\\' {
                            from = start + 1;
                            continue;
                        }
                        let action = rules.actions[hit.pattern().as_usize()];
                        if matches!(action, Action::Line)
                            && rules.line_needs_space
                            && !matches!(before, b' ' | b'\t' | b'\n')
                        {
                            from = start + 1;
                            continue;
                        }
                        if let Action::Str(idx) = action
                            && rules.strings[idx].char
                        {
                            from = start + char_literal_len(&input[start..]).unwrap_or(1);
                            continue;
                        }
                        // Where the opening token ends, and its number of fences.
                        let mut open = (hit.end(), 0);
                        if let Action::Str(idx) = action
                            && let Some(fence) = rules.strings[idx].fence
                        {
                            let string = &rules.strings[idx];
                            let after = &input[start + string.open_len..];
                            let fences = after.iter().take_while(|b| **b == fence).count();
                            if fences > MAX_FENCES || !after[fences..].starts_with(&string.close) {
                                from = start + 1;
                                continue;
                            }
                            let quote = start + string.open_len + fences;
                            open = (quote + string.close.len(), fences);
                        }
                        break Some((hit, action, open));
                    };
                    let Some((hit, action, (open_end, fences))) = found else {
                        let end = limit.max(from).min(len);
                        if end <= pos {
                            break;
                        }
                        push(out, Kind::Code, end - pos);
                        pos = end;
                        break;
                    };
                    match action {
                        Action::Line if rules.kept(&input[hit.end()..]) => {
                            push(out, Kind::Code, hit.end() - pos);
                            self.state = State::SkipLine;
                            pos = hit.end();
                        }
                        Action::Line => {
                            push(out, Kind::Code, hit.start() - pos);
                            push(out, Kind::LineOpen, hit.len());
                            self.state = State::Line;
                            pos = hit.end();
                        }
                        Action::Block(idx) => {
                            let block = &rules.blocks[idx];
                            let rest = &input[hit.end()..];
                            let marked = rest.first().is_some_and(|b| block.markers.contains(b))
                                && !rest.starts_with(block.close.needle());
                            push(out, Kind::Code, hit.start() - pos);
                            push(out, Kind::BlockOpen, hit.len() + marked as usize);
                            self.state = State::Block { idx, depth: 1 };
                            pos = hit.end() + marked as usize;
                        }
                        Action::Str(idx) => {
                            push(out, Kind::Code, open_end - pos);
                            self.state = State::Str { idx, fences };
                            pos = open_end;
                        }
                    }
                }
                State::SkipLine => match memchr::memchr(b'\n', &input[pos..]) {
                    Some(i) => {
                        push(out, Kind::Code, i + 1);
                        self.state = State::Code;
                        pos += i + 1;
                    }
                    None => {
                        push(out, Kind::Code, len - pos);
                        pos = len;
                    }
                },
                State::Line => match memchr::memchr(b'\n', &input[pos..]) {
                    Some(i) => {
                        push(out, Kind::Text, i);
                        self.state = State::Code;
                        pos += i;
                    }
                    None => {
                        push(out, Kind::Text, len - pos);
                        pos = len;
                    }
                },
                State::Block { idx, mut depth } => {
                    let block = &rules.blocks[idx];
                    let (open, close) = (block.open.needle().len(), block.close.needle().len());
                    let mut at = pos;
                    let closed = loop {
                        let end = block.close.find(&input[at..]).map(|i| at + i);
                        let nested = match block.nested {
                            true => block.open.find(&input[at..end.unwrap_or(len)]),
                            false => None,
                        };
                        match (nested, end) {
                            (Some(i), _) => {
                                depth += 1;
                                at += i + open;
                            }
                            (None, Some(end)) => {
                                depth -= 1;
                                at = end + close;
                                if depth == 0 {
                                    break Some(end);
                                }
                            }
                            (None, None) => break None,
                        }
                    };
                    match closed {
                        Some(end) => {
                            push(out, Kind::Text, end - pos);
                            push(out, Kind::Close, close);
                            self.state = State::Code;
                            pos = end + close;
                        }
                        None => {
                            self.state = State::Block { idx, depth };
                            // The tail may hold the first bytes of a token.
                            let tail = if eof { 0 } else { open.max(close) - 1 };
                            let end = len.saturating_sub(tail).max(at);
                            if end <= pos {
                                break;
                            }
                            push(out, Kind::Text, end - pos);
                            pos = end;
                            break;
                        }
                    }
                }
                State::Str { idx, fences } => {
                    let string = &rules.strings[idx];
                    let quote = string.close[0];
                    let escape = string.escape.unwrap_or(quote);
                    let newline = if string.multiline { quote } else { b'\n' };
                    let mut at = pos;
                    // Where the string ends, and whether it ended.
                    let (end, done) = loop {
                        let Some(i) = memchr::memchr3(quote, escape, newline, &input[at..]) else {
                            break (len, false);
                        };
                        let i = at + i;
                        if input[i] == b'\n' && !string.multiline {
                            break (i, true);
                        }
                        if string.escape == Some(input[i]) {
                            if i + 1 >= len && !eof {
                                break (i, false);
                            }
                            at = (i + 2).min(len);
                            continue;
                        }
                        let fenced = i + string.close.len();
                        let end = fenced + fences;
                        if end > len && !eof {
                            break (i, false);
                        }
                        if input[i..].starts_with(&string.close)
                            && input.get(fenced..end).is_some_and(|tail| {
                                tail.iter().all(|byte| Some(*byte) == string.fence)
                            })
                        {
                            break (end, true);
                        }
                        at = i + 1;
                    };
                    push(out, Kind::Code, end - pos);
                    pos = end;
                    if !done {
                        break;
                    }
                    self.state = State::Code;
                }
            }
        }
        if pos > 0 {
            self.prev = input[pos - 1];
        }
        Ok(pos)
    }
}

/// The length of the character literal at the start of `input`, which
/// begins with the quote, or `None` if this is not a literal.
fn char_literal_len(input: &[u8]) -> Option<usize> {
    let quote = input[0];
    match *input.get(1)? {
        b'\\' => {
            let window = &input[..input.len().min(MAX_CHAR_LITERAL)];
            let close = memchr::memchr(quote, window.get(3..)?)?;
            Some(3 + close + 1)
        }
        b'\n' => None,
        first if first == quote => None,
        first => {
            let width = match first {
                0x00..=0x7f => 1,
                0x80..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => 4,
            };
            (input.get(1 + width) == Some(&quote)).then_some(width + 2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::testing::comments;

    fn lexer(toml: &str) -> RuleLexer {
        let def: Definition = toml::from_str(toml).unwrap();
        RuleLexer::new(Arc::new(Rules::compile(&def).unwrap()))
    }

    const C_LIKE: &str = r#"
        name = "Test"
        extensions = ["t"]
        line_comments = ["//"]
        keep = ["lint:"]
        [[block_comments]]
        open = "/*"
        close = "*/"
        nested = true
        doc_markers = ["*"]
        [[strings]]
        open = "\""
        escape = "\\"
        [[strings]]
        open = "'"
        char = true
    "#;

    #[test]
    fn finds_line_and_block_comments() {
        let found = comments(&mut lexer(C_LIKE), "a; // one\nb; /* two */ c;", 1 << 20);
        assert_eq!(
            found,
            [
                (Kind::LineOpen, "//".into()),
                (Kind::Text, " one".into()),
                (Kind::BlockOpen, "/*".into()),
                (Kind::Text, " two ".into()),
                (Kind::Close, "*/".into()),
            ]
        );
    }

    #[test]
    fn ignores_tokens_in_strings_and_chars() {
        let src = r#"let a = "// no \" /* no */"; let b = '"'; let c: &'a str = "//"; // yes"#;
        let found = comments(&mut lexer(C_LIKE), src, 1 << 20);
        assert_eq!(
            found,
            [(Kind::LineOpen, "//".into()), (Kind::Text, " yes".into())]
        );
    }

    #[test]
    fn handles_nesting_and_doc_markers() {
        let found = comments(&mut lexer(C_LIKE), "/** a /* b */ c */ x /**/", 1 << 20);
        assert_eq!(
            found,
            [
                (Kind::BlockOpen, "/**".into()),
                (Kind::Text, " a /* b */ c ".into()),
                (Kind::Close, "*/".into()),
                (Kind::BlockOpen, "/*".into()),
                (Kind::Close, "*/".into()),
            ]
        );
    }

    #[test]
    fn keeps_directives_and_shebang() {
        let src = "#!/bin/x // not this\n// lint: off\n// real\n";
        let found = comments(&mut lexer(C_LIKE), src, 1 << 20);
        assert_eq!(
            found,
            [(Kind::LineOpen, "//".into()), (Kind::Text, " real".into())]
        );
    }

    #[test]
    fn result_does_not_depend_on_window_size() {
        let src = "/** doc */ fn a<'a>() { let s = \"a\\\"//b\"; } // tail\n/* x /* y */ z */\n"
            .repeat(20);
        let whole = comments(&mut lexer(C_LIKE), &src, 1 << 20);
        assert_eq!(whole.len(), 20 * 8);
        for chunk in [1, 2, 3, 7, 31, 64] {
            assert_eq!(
                comments(&mut lexer(C_LIKE), &src, chunk),
                whole,
                "chunk {chunk}"
            );
        }
    }

    #[test]
    fn rejects_broken_definitions() {
        let no_ext: Definition = toml::from_str("name = 'x'\nline_comments = ['#']").unwrap();
        assert!(Rules::compile(&no_ext).is_err());
        let twice = "name = 'x'\nextensions = ['x']\nline_comments = ['#', '#']";
        assert!(Rules::compile(&toml::from_str(twice).unwrap()).is_err());
    }
}
