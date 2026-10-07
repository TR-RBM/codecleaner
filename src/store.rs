//! The docs side: Markdown files that hold the comments, and the index
//! that records which refid lives in which of them.
//!
//! The index is a tab-separated text file, one `refid<TAB>doc<TAB>source`
//! line per refid, and is only ever appended to. A Markdown entry is
//!
//! ````text
//! ## refid: 12
//!
//! ```text
//! // the original comment, byte for byte
//! ```
//! ````
//!
//! The comment is stored verbatim so that `--restore` can put it back.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, ErrorKind, Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};

const INDEX_NAME: &str = "index.tsv";

/// Where the index for a `--docs` path lives.
pub fn index_path(docs: &Path) -> PathBuf {
    if is_single_file(docs) {
        let mut name = docs.file_name().unwrap_or_default().to_os_string();
        name.push(".");
        name.push(INDEX_NAME);
        docs.with_file_name(name)
    } else {
        docs.join(INDEX_NAME)
    }
}

/// Whether `--docs` names one Markdown file rather than a folder.
pub fn is_single_file(docs: &Path) -> bool {
    if docs.exists() {
        return docs.is_file();
    }
    let trailing_slash = docs.as_os_str().to_string_lossy().ends_with(['/', '\\']);
    !trailing_slash
        && docs
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

pub struct Store {
    docs: PathBuf,
    single_file: bool,
    index_path: PathBuf,
    /// refid to position in `doc_names`.
    ids: HashMap<u64, u32>,
    doc_names: Vec<String>,
    doc_numbers: HashMap<String, u32>,
    next_id: u64,
    /// Doc and source of the file being processed.
    current: (u32, String),
    doc_out: Option<(u32, BufWriter<File>)>,
    index_out: Option<BufWriter<File>>,
    /// The doc that `lookup` read from last.
    reader: Option<DocReader>,
}

impl Store {
    /// Opens the docs location and loads its index if there is one. Nothing
    /// is created on disk before the first refid is written.
    pub fn open(docs: &Path) -> Result<Self> {
        let mut store = Store {
            docs: docs.to_path_buf(),
            single_file: is_single_file(docs),
            index_path: index_path(docs),
            ids: HashMap::new(),
            doc_names: Vec::new(),
            doc_numbers: HashMap::new(),
            next_id: 1,
            current: (0, String::new()),
            doc_out: None,
            index_out: None,
            reader: None,
        };
        if store.index_path.exists() {
            let path = store.index_path.clone();
            let file =
                File::open(&path).with_context(|| format!("cannot read {}", path.display()))?;
            for line in BufReader::new(file).lines() {
                let line = line.with_context(|| format!("cannot read {}", path.display()))?;
                let mut fields = line.split('\t');
                let id = fields.next().and_then(|id| id.parse::<u64>().ok());
                if let (Some(id), Some(doc)) = (id, fields.next()) {
                    // The index is data from disk; a restore must not be
                    // led to read files that are not docs.
                    let inside = Path::new(doc)
                        .components()
                        .all(|part| matches!(part, Component::Normal(_)));
                    ensure!(
                        inside,
                        "{} names a doc outside of the docs folder: {doc}",
                        path.display()
                    );
                    let doc = store.doc_number(doc);
                    store.remember(id, doc);
                }
            }
        }
        Ok(store)
    }

    /// Sets the doc that new refids go to. `doc` is relative to the docs
    /// folder and ignored when the docs location is a single file.
    pub fn begin_file(&mut self, doc: &Path, source: &Path) {
        let doc = match self.single_file {
            true => self
                .docs
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            false => doc.to_string_lossy().replace('\\', "/"),
        };
        let source = source.to_string_lossy().replace(['\t', '\n'], " ");
        self.current = (self.doc_number(&doc), source);
    }

    pub fn contains(&self, id: u64) -> bool {
        self.ids.contains_key(&id)
    }

    /// Stores a comment under a new refid.
    pub fn add(&mut self, comment: &str) -> Result<u64> {
        let id = self.next_id;
        let longest_run = comment.split(|c| c != '`').map(str::len).max().unwrap_or(0);
        let fence = "`".repeat(longest_run.max(2) + 1);
        self.write(
            id,
            &format!("## refid: {id}\n\n{fence}text\n{comment}\n{fence}\n\n"),
        )?;
        Ok(id)
    }

    /// Indexes a refid that was found in the source without any text.
    pub fn add_existing(&mut self, id: u64) -> Result<()> {
        self.write(id, &format!("## refid: {id}\n\n"))
    }

    /// The comment stored for a refid, if there is one.
    pub fn lookup(&mut self, id: u64) -> Result<Option<String>> {
        let Some(&doc) = self.ids.get(&id) else {
            return Ok(None);
        };
        if self.reader.as_ref().is_none_or(|reader| reader.doc != doc) {
            let path = self.doc_path(doc);
            self.reader = match File::open(&path) {
                Ok(file) => Some(DocReader::new(doc, file)?),
                Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
                Err(err) => {
                    return Err(err).with_context(|| format!("cannot read {}", path.display()));
                }
            };
        }
        match &mut self.reader {
            Some(reader) => reader.read(id),
            None => Ok(None),
        }
    }

    /// Writes everything buffered to disk.
    pub fn flush(&mut self) -> Result<()> {
        if let Some((_, out)) = &mut self.doc_out {
            out.flush()?;
        }
        if let Some(out) = &mut self.index_out {
            out.flush()?;
        }
        Ok(())
    }

    fn write(&mut self, id: u64, entry: &str) -> Result<()> {
        let doc = self.current.0;
        if self.doc_out.as_ref().is_none_or(|(open, _)| *open != doc) {
            self.flush()?;
            self.doc_out = Some((doc, append(&self.doc_path(doc))?));
        }
        if self.index_out.is_none() {
            self.index_out = Some(append(&self.index_path)?);
        }
        let (Some((_, doc_out)), Some(index_out)) = (&mut self.doc_out, &mut self.index_out) else {
            unreachable!("both writers were opened above");
        };
        doc_out.write_all(entry.as_bytes())?;
        writeln!(
            index_out,
            "{id}\t{}\t{}",
            self.doc_names[doc as usize], self.current.1
        )?;
        self.remember(id, doc);
        Ok(())
    }

    fn remember(&mut self, id: u64, doc: u32) {
        self.ids.insert(id, doc);
        self.next_id = self.next_id.max(id.saturating_add(1));
    }

    fn doc_number(&mut self, name: &str) -> u32 {
        if let Some(&number) = self.doc_numbers.get(name) {
            return number;
        }
        let number = self.doc_names.len() as u32;
        self.doc_names.push(name.to_string());
        self.doc_numbers.insert(name.to_string(), number);
        number
    }

    fn doc_path(&self, doc: u32) -> PathBuf {
        match self.single_file {
            true => self.docs.clone(),
            false => self.docs.join(&self.doc_names[doc as usize]),
        }
    }
}

fn append(path: &Path) -> Result<BufWriter<File>> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(BufWriter::new(file))
}

/// Parses `refid: 123`, the text of a comment that was already replaced.
pub fn parse_refid(text: &[u8]) -> Option<u64> {
    let digits = text
        .trim_ascii()
        .strip_prefix(b"refid:")?
        .trim_ascii_start();
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// A Markdown doc opened for `--restore`. Only the position of each
/// comment is kept in memory; the text is read when it is asked for.
struct DocReader {
    doc: u32,
    file: BufReader<File>,
    /// Where `file` currently is.
    pos: u64,
    /// refid to offset and length of its comment.
    entries: HashMap<u64, (u64, usize)>,
}

impl DocReader {
    fn new(doc: u32, file: File) -> Result<Self> {
        let mut file = BufReader::new(file);
        let entries = scan_doc(&mut file)?;
        let pos = file.stream_position()?;
        Ok(DocReader {
            doc,
            file,
            pos,
            entries,
        })
    }

    fn read(&mut self, id: u64) -> Result<Option<String>> {
        let Some(&(offset, len)) = self.entries.get(&id) else {
            return Ok(None);
        };
        // Refids are mostly asked for in the order they were written, and a
        // relative seek keeps the read buffer in that case.
        self.file.seek_relative(offset as i64 - self.pos as i64)?;
        let mut text = vec![0; len];
        self.file.read_exact(&mut text)?;
        self.pos = offset + len as u64;
        Ok(String::from_utf8(text).ok())
    }
}

/// Finds all entries with a comment in a Markdown doc.
fn scan_doc(doc: &mut impl BufRead) -> Result<HashMap<u64, (u64, usize)>> {
    let mut entries = HashMap::new();
    let mut line = Vec::new();
    let mut pos = 0u64;
    // Reads the next line, without its `\n`, and returns where it started.
    let mut next = |line: &mut Vec<u8>| -> Result<Option<u64>> {
        line.clear();
        let start = pos;
        let read = doc.read_until(b'\n', line)?;
        pos += read as u64;
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        Ok((read > 0).then_some(start))
    };
    let mut pending = next(&mut line)?;
    while pending.is_some() {
        let Some(id) = line.strip_prefix(b"## ").and_then(parse_refid) else {
            pending = next(&mut line)?;
            continue;
        };
        pending = next(&mut line)?;
        if pending.is_some() && line.trim_ascii().is_empty() {
            pending = next(&mut line)?;
        }
        let ticks = line.iter().take_while(|b| **b == b'`').count();
        if pending.is_none() || ticks < 3 || &line[ticks..] != b"text" {
            // An entry without text; this line may be the next heading.
            continue;
        }
        let fence = line[..ticks].to_vec();
        let mut text = None;
        loop {
            pending = next(&mut line)?;
            let Some(start) = pending else {
                break;
            };
            let first = *text.get_or_insert(start);
            // The stored text has no run of backticks this long, so the
            // first line that starts with one is the closing fence.
            if line.starts_with(&fence) {
                entries.insert(id, (first, (start - first).saturating_sub(1) as usize));
                break;
            }
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_refids() {
        assert_eq!(parse_refid(b" refid: 1234 "), Some(1234));
        assert_eq!(parse_refid(b"refid:7\r"), Some(7));
        assert_eq!(parse_refid(b" refid: 12 and more"), None);
        assert_eq!(parse_refid(b" a comment"), None);
    }

    #[test]
    fn stores_and_finds_comments() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        let tricky = "/* has ```` backticks\n```text\n## refid: 99\n */";
        {
            let mut store = Store::open(&docs).unwrap();
            store.begin_file(Path::new("src/main.md"), Path::new("src/main.rs"));
            assert_eq!(store.add("// first\r\n    // second").unwrap(), 1);
            assert_eq!(store.add(tricky).unwrap(), 2);
            assert_eq!(store.add("").unwrap(), 3);
            store.add_existing(40).unwrap();
            assert_eq!(store.add("// after").unwrap(), 41);
            store.flush().unwrap();
        }
        assert!(docs.join("src/main.md").is_file());

        let mut store = Store::open(&docs).unwrap();
        assert!(store.contains(40) && !store.contains(99));
        assert_eq!(
            store.lookup(1).unwrap().as_deref(),
            Some("// first\r\n    // second")
        );
        assert_eq!(store.lookup(41).unwrap().as_deref(), Some("// after"));
        assert_eq!(store.lookup(2).unwrap().as_deref(), Some(tricky));
        assert_eq!(store.lookup(3).unwrap().as_deref(), Some(""));
        assert_eq!(store.lookup(40).unwrap(), None);
        assert_eq!(store.lookup(41).unwrap().as_deref(), Some("// after"));
        store.begin_file(Path::new("other.md"), Path::new("other.rs"));
        assert_eq!(store.add("# next").unwrap(), 42);
    }

    #[test]
    fn rejects_index_entries_outside_of_the_docs() {
        let dir = tempfile::tempdir().unwrap();
        for doc in ["../secret.md", "/etc/passwd", "a/../../b.md"] {
            fs::write(dir.path().join("index.tsv"), format!("1\t{doc}\tmain.rs\n")).unwrap();
            assert!(Store::open(dir.path()).is_err(), "{doc}");
        }
    }

    #[test]
    fn docs_can_be_a_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("notes.md");
        let mut store = Store::open(&docs).unwrap();
        store.begin_file(Path::new("a/b.md"), Path::new("a/b.rs"));
        store.add("// hi").unwrap();
        store.flush().unwrap();
        assert!(docs.is_file());
        assert!(dir.path().join("notes.md.index.tsv").is_file());
    }
}
