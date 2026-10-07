//! Rewrites one file as a stream: the host reads and writes, a [`Lexer`]
//! says which bytes are comments, and the [`Store`] keeps their text.
//!
//! Memory use is bounded by the read window plus the comment being read;
//! the file itself is never held in memory.

use std::fs::{self, File};
use std::io::{BufWriter, ErrorKind, Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use crate::lexer::{Kind, Lexer};
use crate::store::{Store, parse_refid};

/// Size of the read window and of the write buffer.
const WINDOW: usize = 256 * 1024;
/// A comment larger than this is left in place rather than buffered.
const MAX_COMMENT: usize = 8 * 1024 * 1024;
/// Indentation longer than this ends a group of line comments.
const MAX_INDENT: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Replace comments with refids.
    Clean,
    /// Replace refids with the comments stored for them.
    Restore,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub files: u64,
    /// Comment sections that are a refid after the run, or were one before
    /// a restore.
    pub refids: u64,
    /// Refids added to the index in this run.
    pub indexed: u64,
    pub restored: u64,
    /// Refids that could not be restored because the docs have no text.
    pub missing: u64,
}

/// Rewrites `path` in place. The file is replaced atomically, and only if
/// something changed.
pub fn process_file(
    path: &Path,
    lexer: &mut dyn Lexer,
    store: &mut Store,
    mode: Mode,
    counts: &mut Counts,
) -> Result<()> {
    let mut source = File::open(path)?;
    let permissions = source.metadata()?.permissions();
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temp = tempfile::Builder::new()
        .prefix(".codecleanup-")
        .tempfile_in(dir)?;
    let out = BufWriter::with_capacity(WINDOW, temp.as_file());
    let changed = rewrite(&mut source, out, lexer, store, mode, counts)?;
    // Windows does not replace a file that is still open.
    drop(source);
    // The docs go to disk first: a crash in between leaves an unused entry
    // behind, never a refid without its text.
    store.flush()?;
    if changed {
        fs::set_permissions(temp.path(), permissions)?;
        temp.persist(path)?;
    }
    counts.files += 1;
    Ok(())
}

/// Copies `source` to `out` with the comments replaced. Returns whether
/// the output differs from the input.
pub fn rewrite(
    source: &mut dyn Read,
    out: impl Write,
    lexer: &mut dyn Lexer,
    store: &mut Store,
    mode: Mode,
    counts: &mut Counts,
) -> Result<bool> {
    lexer.reset();
    let mut rewriter = Rewriter {
        out,
        store,
        mode,
        counts,
        changed: false,
        line_blank: true,
        indent: Vec::new(),
        comment: None,
        group: None,
        held: Vec::new(),
    };
    let mut window = vec![0u8; WINDOW];
    let (mut start, mut end) = (0, 0);
    let mut eof = false;
    let mut segments = Vec::new();
    loop {
        if !eof && end < window.len() {
            match source.read(&mut window[end..]) {
                Ok(0) => eof = true,
                Ok(n) => end += n,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => return Err(err.into()),
            }
        }
        segments.clear();
        let used = lexer.scan(&window[start..end], eof, &mut segments)?;
        let covered: usize = segments.iter().map(|segment| segment.len).sum();
        ensure!(
            covered == used && used <= end - start,
            "the language plugin returned broken segments"
        );
        for segment in &segments {
            let bytes = &window[start..start + segment.len];
            match segment.kind {
                Kind::Code => rewriter.code(bytes)?,
                Kind::LineOpen => rewriter.open(bytes, false)?,
                Kind::BlockOpen => rewriter.open(bytes, true)?,
                Kind::Text => rewriter.text(bytes)?,
                Kind::Close => rewriter.close(bytes)?,
            }
            start += segment.len;
        }
        if start == end {
            if eof {
                break;
            }
            (start, end) = (0, 0);
        } else if eof {
            ensure!(
                used > 0,
                "the language plugin stopped before the end of the file"
            );
        } else if end == window.len() {
            if start == 0 {
                bail!("the language plugin needs more than {WINDOW} bytes of lookahead");
            }
            window.copy_within(start..end, 0);
            (start, end) = (0, end - start);
        }
    }
    rewriter.finish()
}

/// A comment while its segments arrive.
struct Comment {
    block: bool,
    open: Vec<u8>,
    text: Vec<u8>,
    /// Nothing but whitespace precedes it on its line.
    own_line: bool,
    /// The whitespace before it, if it is on its own line.
    indent: Vec<u8>,
    /// Too large to buffer: already written out, the rest follows as is.
    passthrough: bool,
}

/// Consecutive full-line comments with the same token and indentation,
/// which become one refid.
struct Group {
    open: Vec<u8>,
    indent: Vec<u8>,
    /// The comments exactly as they are in the file.
    raw: Vec<u8>,
    /// The last line ends in `\r`, which is not part of `raw`.
    carriage_return: bool,
}

struct Rewriter<'a, W: Write> {
    out: W,
    store: &'a mut Store,
    mode: Mode,
    counts: &'a mut Counts,
    changed: bool,
    /// Only whitespace was seen since the last newline, and it is in `indent`.
    line_blank: bool,
    indent: Vec<u8>,
    comment: Option<Comment>,
    /// A group that the next line comment may still join.
    group: Option<Group>,
    /// The code after `group`: a newline and the indentation that follows.
    held: Vec<u8>,
}

impl<W: Write> Rewriter<'_, W> {
    fn code(&mut self, mut bytes: &[u8]) -> Result<()> {
        self.end_line_comment()?;
        let line = match memchr::memrchr(b'\n', bytes) {
            Some(i) => {
                self.line_blank = true;
                self.indent.clear();
                &bytes[i + 1..]
            }
            None => bytes,
        };
        self.line_blank = self.line_blank
            && self.indent.len() + line.len() <= MAX_INDENT
            && line.iter().all(|byte| matches!(byte, b' ' | b'\t'));
        if self.line_blank {
            self.indent.extend_from_slice(line);
        }
        if self.group.is_some() {
            // Hold back `\n` plus indentation; anything else ends the group.
            let fits = bytes
                .iter()
                .enumerate()
                .take_while(|&(i, &byte)| match self.held.len() + i {
                    0 => byte == b'\n',
                    held => held < MAX_INDENT && matches!(byte, b' ' | b'\t'),
                })
                .count();
            self.held.extend_from_slice(&bytes[..fits]);
            bytes = &bytes[fits..];
            if bytes.is_empty() {
                return Ok(());
            }
            self.flush_group()?;
        }
        self.out.write_all(bytes)?;
        Ok(())
    }

    fn open(&mut self, token: &[u8], block: bool) -> Result<()> {
        self.end_line_comment()?;
        if block {
            self.flush_group()?;
        }
        self.comment = Some(Comment {
            block,
            open: token.to_vec(),
            text: Vec::new(),
            own_line: self.line_blank,
            indent: if self.line_blank {
                self.indent.clone()
            } else {
                Vec::new()
            },
            passthrough: false,
        });
        self.line_blank = false;
        Ok(())
    }

    fn text(&mut self, bytes: &[u8]) -> Result<()> {
        let Some(comment) = &mut self.comment else {
            return self.code(bytes);
        };
        if comment.passthrough {
            self.out.write_all(bytes)?;
            return Ok(());
        }
        comment.text.extend_from_slice(bytes);
        if comment.text.len() > MAX_COMMENT {
            comment.passthrough = true;
            let seen = [&comment.open[..], &comment.text].concat();
            comment.text = Vec::new();
            self.flush_group()?;
            self.out.write_all(&seen)?;
        }
        Ok(())
    }

    fn close(&mut self, token: &[u8]) -> Result<()> {
        let Some(comment) = self.comment.take().filter(|comment| comment.block) else {
            return self.code(token);
        };
        if comment.passthrough {
            self.out.write_all(token)?;
            return Ok(());
        }
        let raw = [&comment.open[..], &comment.text, token].concat();
        match parse_refid(&comment.text) {
            Some(id) => self.existing(id, &raw),
            None if self.mode == Mode::Clean => self.replace(&comment.open, &raw, token),
            None => Ok(self.out.write_all(&raw)?),
        }
    }

    /// A line comment has no closing token; it ends when something else
    /// follows.
    fn end_line_comment(&mut self) -> Result<()> {
        let Some(mut comment) = self.comment.take_if(|comment| !comment.block) else {
            return Ok(());
        };
        if comment.passthrough {
            return Ok(());
        }
        let carriage_return = comment.text.last() == Some(&b'\r');
        if carriage_return {
            comment.text.pop();
        }
        let raw = [&comment.open[..], &comment.text].concat();
        let refid = parse_refid(&comment.text);
        let joins = self.group.as_ref().is_some_and(|group| {
            refid.is_none()
                && comment.own_line
                && group.open == comment.open
                && self.held.strip_prefix(b"\n") == Some(&group.indent[..])
        });
        if !joins {
            self.flush_group()?;
        }
        if let Some(id) = refid {
            self.existing(id, &raw)?;
        } else if self.mode == Mode::Restore {
            self.out.write_all(&raw)?;
        } else if !comment.own_line {
            self.replace(&comment.open, &raw, b"")?;
        } else if let Some(group) = self.group.as_mut().filter(|_| joins) {
            if group.carriage_return {
                group.raw.push(b'\r');
            }
            group.raw.append(&mut self.held);
            group.raw.extend_from_slice(&raw);
            group.carriage_return = carriage_return;
            return Ok(());
        } else {
            let Comment { open, indent, .. } = comment;
            self.group = Some(Group {
                open,
                indent,
                raw,
                carriage_return,
            });
            return Ok(());
        }
        if carriage_return {
            self.out.write_all(b"\r")?;
        }
        Ok(())
    }

    /// Writes the refid for a finished group, then the code held behind it.
    fn flush_group(&mut self) -> Result<()> {
        if let Some(group) = self.group.take() {
            self.replace(&group.open, &group.raw, b"")?;
            if group.carriage_return {
                self.out.write_all(b"\r")?;
            }
        }
        self.out.write_all(&self.held)?;
        self.held.clear();
        Ok(())
    }

    /// Moves a comment into the docs and writes its refid instead.
    fn replace(&mut self, open: &[u8], raw: &[u8], close: &[u8]) -> Result<()> {
        // The docs are text; a comment that is not is better left alone.
        let Ok(comment) = std::str::from_utf8(raw) else {
            return Ok(self.out.write_all(raw)?);
        };
        let id = self.store.add(comment)?;
        self.out.write_all(open)?;
        write!(self.out, " refid: {id}")?;
        if !close.is_empty() {
            self.out.write_all(b" ")?;
            self.out.write_all(close)?;
        }
        self.counts.refids += 1;
        self.counts.indexed += 1;
        self.changed = true;
        Ok(())
    }

    /// Handles a comment that already is a refid.
    fn existing(&mut self, id: u64, raw: &[u8]) -> Result<()> {
        self.counts.refids += 1;
        match self.mode {
            Mode::Clean => {
                if !self.store.contains(id) {
                    self.store.add_existing(id)?;
                    self.counts.indexed += 1;
                }
            }
            Mode::Restore => match self.store.lookup(id)? {
                Some(comment) => {
                    self.counts.restored += 1;
                    self.changed = true;
                    return Ok(self.out.write_all(comment.as_bytes())?);
                }
                None => self.counts.missing += 1,
            },
        }
        Ok(self.out.write_all(raw)?)
    }

    fn finish(mut self) -> Result<bool> {
        self.end_line_comment()?;
        self.flush_group()?;
        // A block comment that never closes is left as it is.
        if let Some(comment) = self.comment.take().filter(|comment| !comment.passthrough) {
            self.out.write_all(&comment.open)?;
            self.out.write_all(&comment.text)?;
        }
        self.out
            .flush()
            .context("cannot write the rewritten file")?;
        Ok(self.changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::Registry;

    /// Runs one mode over `src` as a file named `name` and returns the result.
    fn run(docs: &Path, name: &str, src: &str, mode: Mode) -> (String, Counts) {
        let mut registry = Registry::load(&[]).unwrap();
        let lexer = registry.lexer_for(Path::new(name)).unwrap();
        let mut store = Store::open(docs).unwrap();
        store.begin_file(Path::new("out.md"), Path::new(name));
        let mut counts = Counts::default();
        let mut out = Vec::new();
        rewrite(
            &mut src.as_bytes(),
            &mut out,
            lexer,
            &mut store,
            mode,
            &mut counts,
        )
        .unwrap();
        store.flush().unwrap();
        (String::from_utf8(out).unwrap(), counts)
    }

    #[test]
    fn replaces_trailing_comment() {
        let dir = tempfile::tempdir().unwrap();
        let src = "fn main() {\n    let lucky_number = 7; // I'm feeling lucky today\n}\n";
        let (out, counts) = run(dir.path(), "main.rs", src, Mode::Clean);
        assert_eq!(
            out,
            "fn main() {\n    let lucky_number = 7; // refid: 1\n}\n"
        );
        assert_eq!((counts.refids, counts.indexed), (1, 1));
        let doc = fs::read_to_string(dir.path().join("out.md")).unwrap();
        assert_eq!(
            doc,
            "## refid: 1\n\n```text\n// I'm feeling lucky today\n```\n\n"
        );
    }

    #[test]
    fn groups_consecutive_line_comments() {
        let dir = tempfile::tempdir().unwrap();
        let src = "    // one\n    // two\n    // three\n\n    // alone\n    x(); // tail\n    // below\n/// doc\n// plain\n";
        let (out, counts) = run(dir.path(), "a.rs", src, Mode::Clean);
        assert_eq!(
            out,
            "    // refid: 1\n\n    // refid: 2\n    x(); // refid: 3\n    // refid: 4\n/// refid: 5\n// refid: 6\n"
        );
        assert_eq!(counts.refids, 6);
    }

    #[test]
    fn replaces_block_comments() {
        let dir = tempfile::tempdir().unwrap();
        let src = "/* a\n * b\n */\nint x; /** doc */ int y; /* open";
        let (out, _) = run(dir.path(), "a.c", src, Mode::Clean);
        assert_eq!(out, "/* refid: 1 */\nint x; /** refid: 2 */ int y; /* open");
    }

    #[test]
    fn second_run_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _) = run(dir.path(), "a.py", "# a\n# b\nx = 1  # c\n", Mode::Clean);
        assert_eq!(first, "# refid: 1\nx = 1  # refid: 2\n");
        let (second, counts) = run(dir.path(), "a.py", &first, Mode::Clean);
        assert_eq!(second, first);
        assert_eq!((counts.refids, counts.indexed), (2, 0));
    }

    #[test]
    fn indexes_unknown_refids_once() {
        let dir = tempfile::tempdir().unwrap();
        let src = "// refid: 50\n// new\n/* refid: 60 */\n";
        let (out, counts) = run(dir.path(), "a.rs", src, Mode::Clean);
        assert_eq!(out, "// refid: 50\n// refid: 51\n/* refid: 60 */\n");
        assert_eq!((counts.refids, counts.indexed), (3, 3));
        let (_, counts) = run(dir.path(), "a.rs", &out, Mode::Clean);
        assert_eq!((counts.refids, counts.indexed), (3, 0));
    }

    #[test]
    fn restore_brings_the_file_back() {
        let dir = tempfile::tempdir().unwrap();
        let src = "#!/usr/bin/env python3\n# one\r\n    # two\r\nx = \"# no\"  # three\r\n'''\n# doc\n'''\n# last";
        let (cleaned, _) = run(dir.path(), "a.py", src, Mode::Clean);
        assert_eq!(
            cleaned,
            "#!/usr/bin/env python3\n# refid: 1\r\n    # refid: 2\r\nx = \"# no\"  # refid: 3\r\n'''\n# doc\n'''\n# refid: 4"
        );
        let (restored, counts) = run(dir.path(), "a.py", &cleaned, Mode::Restore);
        assert_eq!(restored, src);
        assert_eq!((counts.refids, counts.restored, counts.missing), (4, 4, 0));
    }

    #[test]
    fn restore_leaves_unknown_refids() {
        let dir = tempfile::tempdir().unwrap();
        let (out, counts) = run(dir.path(), "a.rs", "// refid: 9\n// text\n", Mode::Restore);
        assert_eq!(out, "// refid: 9\n// text\n");
        assert_eq!((counts.restored, counts.missing), (0, 1));
    }

    #[test]
    fn large_input_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let unit = "/// Adds things.\n/// Carefully.\nfn add(a: u8) -> u8 { a + 1 } // done\n\n";
        let src = unit.repeat(3 * WINDOW / unit.len());
        let (cleaned, counts) = run(dir.path(), "big.rs", &src, Mode::Clean);
        assert_eq!(counts.refids as usize, 2 * (3 * WINDOW / unit.len()));
        assert!(!cleaned.contains("Carefully"));
        let (restored, _) = run(dir.path(), "big.rs", &cleaned, Mode::Restore);
        assert!(restored == src);
    }
}
