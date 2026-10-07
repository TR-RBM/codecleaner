//! Code Cleanup: replaces the comments in source files with refids and
//! moves their text into a docs folder, and back again with `--restore`.

mod lexer;
mod plugins;
mod process;
mod rules;
mod store;
mod wasm;

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum, ValueHint};
use ignore::WalkBuilder;

use crate::plugins::Registry;
use crate::process::{Counts, Mode, process_file};
use crate::store::Store;

const BASH_COMPLETION: &str = include_str!("../completions/codecleanup.bash");
const FISH_COMPLETION: &str = include_str!("../completions/codecleanup.fish");

/// Code Cleanup - replaces comments in source code with refids.
///
/// Every comment section becomes a short reference such as `// refid: 1234`.
/// The original text is written to the docs location as Markdown, next to
/// an index of all refids. `--restore` puts the comments back.
#[derive(Debug, Parser)]
#[command(name = "codecleanup", version, arg_required_else_help = true)]
struct Cli {
    /// Source file, or folder to process recursively
    #[arg(long = "in", value_name = "PATH", value_hint = ValueHint::AnyPath)]
    input: Option<PathBuf>,

    /// Docs folder, or a single .md file, that receives the comments
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    docs: Option<PathBuf>,

    /// Replace refids with the comments stored in the docs
    #[arg(long)]
    restore: bool,

    /// Additional folder with language plugins (.toml or .wasm)
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
    plugins: Option<PathBuf>,

    /// List the supported languages and exit
    #[arg(long)]
    languages: bool,

    /// Print the completion script for a shell and exit
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Shell {
    Bash,
    Fish,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.completions {
        Some(Shell::Bash) => print!("{BASH_COMPLETION}"),
        Some(Shell::Fish) => print!("{FISH_COMPLETION}"),
        None => {}
    }
    if cli.completions.is_some() {
        return Ok(ExitCode::SUCCESS);
    }

    let mut plugin_dirs = Vec::new();
    if let Some(config) = config_dir() {
        plugin_dirs.push(config.join("codecleanup").join("plugins"));
    }
    plugin_dirs.extend(cli.plugins.as_deref().map(expand_tilde));
    let mut registry = Registry::load(&plugin_dirs)?;
    if cli.languages {
        for plugin in registry.plugins() {
            let mut types: Vec<String> =
                plugin.extensions.iter().map(|e| format!(".{e}")).collect();
            types.extend(plugin.filenames.iter().cloned());
            println!(
                "{:<10} {}  ({})",
                plugin.name,
                types.join(" "),
                plugin.origin
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    let (Some(input), Some(docs)) = (cli.input, cli.docs) else {
        bail!("--in and --docs are both required (see --help)");
    };
    let (mut input, mut docs) = (expand_tilde(&input), expand_tilde(&docs));
    let mode = if cli.restore {
        Mode::Restore
    } else {
        Mode::Clean
    };
    // A restore needs an index; if only --in has one, the two were mixed up.
    if mode == Mode::Restore
        && !store::index_path(&docs).is_file()
        && store::index_path(&input).is_file()
    {
        eprintln!("note: the index is in --in, so --in and --docs are treated as swapped");
        std::mem::swap(&mut input, &mut docs);
    }
    if mode == Mode::Restore && !store::index_path(&docs).is_file() {
        bail!("no index found at {}", store::index_path(&docs).display());
    }

    let started = Instant::now();
    // Resolving the path also makes a symlinked file itself get rewritten.
    let input = input
        .canonicalize()
        .with_context(|| format!("cannot read {}", input.display()))?;
    let mut store = Store::open(&docs)?;
    let mut counts = Counts::default();

    if input.is_file() {
        let Some(lexer) = registry.lexer_for(&input) else {
            println!("filetype not supported");
            return Ok(ExitCode::FAILURE);
        };
        let name = PathBuf::from(input.file_name().unwrap_or_default());
        store.begin_file(&name.with_extension("md"), &name);
        process_file(&input, lexer, &mut store, mode, &mut counts)
            .with_context(|| format!("cannot process {}", input.display()))?;
    } else {
        // The docs may live inside the input folder; they are not source.
        let docs_dir = docs.canonicalize().ok();
        for entry in WalkBuilder::new(&input)
            .sort_by_file_name(|a, b| a.cmp(b))
            .build()
        {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type().is_some_and(|kind| kind.is_file())
                || docs_dir
                    .as_deref()
                    .is_some_and(|docs| path.starts_with(docs))
            {
                continue;
            }
            let Some(lexer) = registry.lexer_for(path) else {
                continue;
            };
            let relative = path.strip_prefix(&input).unwrap_or(path);
            store.begin_file(&relative.with_extension("md"), relative);
            process_file(path, lexer, &mut store, mode, &mut counts)
                .with_context(|| format!("cannot process {}", path.display()))?;
        }
    }
    store.flush()?;

    println!(
        "processed {} files, with {} refids",
        counts.files, counts.refids
    );
    match mode {
        Mode::Clean => println!("indexed {} refids", counts.indexed),
        Mode::Restore => println!("restored {} refids", counts.restored),
    }
    if counts.missing > 0 {
        println!(
            "left {} refids untouched, the docs have no text for them",
            counts.missing
        );
    }
    println!("run time: {}", human_time(started.elapsed()));
    Ok(ExitCode::SUCCESS)
}

/// Shells do not expand `~` after `--in=`, so it is done here.
fn expand_tilde(path: &Path) -> PathBuf {
    let home = env::var_os("HOME").map(PathBuf::from);
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

fn config_dir() -> Option<PathBuf> {
    let xdg = env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    xdg.or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".config")))
}

fn human_time(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64();
    if seconds < 0.001 {
        format!("{} µs", elapsed.as_micros())
    } else if seconds < 1.0 {
        format!("{:.1} ms", seconds * 1000.0)
    } else if seconds < 60.0 {
        format!("{seconds:.2} seconds")
    } else if seconds < 3600.0 {
        format!(
            "{} minutes {} seconds",
            elapsed.as_secs() / 60,
            elapsed.as_secs() % 60
        )
    } else {
        format!(
            "{} hours {} minutes",
            elapsed.as_secs() / 3600,
            elapsed.as_secs() % 3600 / 60
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_run_time() {
        assert_eq!(human_time(Duration::from_micros(250)), "250 µs");
        assert_eq!(human_time(Duration::from_millis(12)), "12.0 ms");
        assert_eq!(human_time(Duration::from_millis(2500)), "2.50 seconds");
        assert_eq!(human_time(Duration::from_secs(125)), "2 minutes 5 seconds");
        assert_eq!(human_time(Duration::from_secs(3720)), "1 hours 2 minutes");
    }

    #[test]
    fn expands_tilde() {
        let home = PathBuf::from(env::var_os("HOME").unwrap());
        assert_eq!(expand_tilde(Path::new("~/docs")), home.join("docs"));
        assert_eq!(expand_tilde(Path::new("./docs")), PathBuf::from("./docs"));
        assert_eq!(
            expand_tilde(Path::new("~user/docs")),
            PathBuf::from("~user/docs")
        );
    }
}
