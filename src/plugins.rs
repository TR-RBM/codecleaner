//! Finds the language plugin for a file.
//!
//! Plugins are either TOML rule files or WebAssembly modules. The built-in
//! ones are compiled into the binary; more are read from plugin folders,
//! where a later plugin wins over an earlier one for the same file type.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::lexer::Lexer;
use crate::rules::{Definition, RuleLexer, Rules};
use crate::wasm;

const BUILTIN_RULES: &[(&str, &str)] = &[
    ("c.toml", include_str!("../languages/c.toml")),
    ("cpp.toml", include_str!("../languages/cpp.toml")),
    ("csharp.toml", include_str!("../languages/csharp.toml")),
    ("make.toml", include_str!("../languages/make.toml")),
    ("python.toml", include_str!("../languages/python.toml")),
    ("rust.toml", include_str!("../languages/rust.toml")),
];

/// Built from `plugins/` with `plugins/build.sh`.
const BUILTIN_WASM: &[(&str, &[u8])] = &[
    ("lua.wasm", include_bytes!("../languages/lua.wasm")),
    (
        "typescript.wasm",
        include_bytes!("../languages/typescript.wasm"),
    ),
];

pub struct Plugin {
    pub name: String,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    /// `built-in` or the path of the plugin file.
    pub origin: String,
    lexer: Box<dyn Lexer>,
}

pub struct Registry {
    plugins: Vec<Plugin>,
    by_extension: HashMap<String, usize>,
    by_filename: HashMap<String, usize>,
}

impl Registry {
    /// Loads the built-in plugins and those in `dirs`. Folders that do not
    /// exist are skipped.
    pub fn load(dirs: &[PathBuf]) -> Result<Self> {
        let mut registry = Registry {
            plugins: Vec::new(),
            by_extension: HashMap::new(),
            by_filename: HashMap::new(),
        };
        for (file, text) in BUILTIN_RULES {
            let plugin =
                rules_plugin(text, "built-in").with_context(|| format!("built-in {file}"))?;
            registry.add(plugin);
        }
        for (file, bytes) in BUILTIN_WASM {
            let plugin =
                wasm_plugin(bytes, "built-in").with_context(|| format!("built-in {file}"))?;
            registry.add(plugin);
        }
        for dir in dirs {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = entries
                .filter_map(|entry| Some(entry.ok()?.path()))
                .collect();
            paths.sort();
            for path in paths {
                let origin = path.display().to_string();
                let plugin = match path.extension().and_then(|ext| ext.to_str()) {
                    Some("toml") => rules_plugin(&fs::read_to_string(&path)?, &origin),
                    Some("wasm") => wasm_plugin(&fs::read(&path)?, &origin),
                    _ => continue,
                };
                registry.add(plugin.with_context(|| format!("cannot load plugin {origin}"))?);
            }
        }
        Ok(registry)
    }

    fn add(&mut self, plugin: Plugin) {
        let number = self.plugins.len();
        for extension in &plugin.extensions {
            self.by_extension
                .insert(extension.to_ascii_lowercase(), number);
        }
        for filename in &plugin.filenames {
            self.by_filename.insert(filename.to_lowercase(), number);
        }
        self.plugins.push(plugin);
    }

    /// The lexer for a file, or `None` if its type is not supported.
    pub fn lexer_for(&mut self, path: &Path) -> Option<&mut dyn Lexer> {
        // A file name wins over the extension: `makefile.inc` is a Makefile.
        let name = path.file_name()?.to_str()?.to_lowercase();
        let number = match self.by_filename.get(&name) {
            Some(number) => *number,
            None => {
                let extension = path.extension()?.to_str()?.to_ascii_lowercase();
                *self.by_extension.get(&extension)?
            }
        };
        Some(self.plugins[number].lexer.as_mut())
    }

    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }
}

fn rules_plugin(text: &str, origin: &str) -> Result<Plugin> {
    let definition: Definition = toml::from_str(text)?;
    let rules = Arc::new(Rules::compile(&definition)?);
    Ok(Plugin {
        name: definition.name,
        extensions: definition.extensions,
        filenames: definition.filenames,
        origin: origin.to_string(),
        lexer: Box::new(RuleLexer::new(rules)),
    })
}

fn wasm_plugin(bytes: &[u8], origin: &str) -> Result<Plugin> {
    let (info, lexer) = wasm::load(bytes)?;
    Ok(Plugin {
        name: info.name,
        extensions: info.extensions,
        filenames: info.filenames,
        origin: origin.to_string(),
        lexer: Box::new(lexer),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Kind;
    use crate::lexer::testing::comments;

    /// The text of every comment the built-in plugin for `file` finds.
    fn texts(file: &str, src: &str) -> Vec<String> {
        let mut registry = Registry::load(&[]).unwrap();
        let lexer = registry.lexer_for(Path::new(file)).unwrap();
        let whole = comments(lexer, src, 1 << 20);
        assert_eq!(
            comments(lexer, src, 5),
            whole,
            "small windows must not change the result"
        );
        whole
            .into_iter()
            .filter(|(kind, _)| *kind == Kind::Text)
            .map(|(_, text)| text)
            .collect()
    }

    #[test]
    fn unknown_types_have_no_lexer() {
        let mut registry = Registry::load(&[]).unwrap();
        assert!(registry.lexer_for(Path::new("notes.txt")).is_none());
        assert!(registry.lexer_for(Path::new("README")).is_none());
        assert!(registry.lexer_for(Path::new("a.tsx")).is_none());
        assert!(registry.lexer_for(Path::new("MAIN.RS")).is_some());
    }

    #[test]
    fn rust() {
        let src = r####"
            //! crate docs
            fn f<'a>(x: &'a str) -> char { // lifetimes
                let _ = r#"// raw "quoted" /* no */"#;
                let _ = "a \" // no"; let r#type = br##"// no "# /* no */"##;
                let _ = ['"', '\'', '/']; /* block /* nested */ still */
                '\u{1F600}' //// four
            }
        "####;
        assert_eq!(
            texts("lib.rs", src),
            [
                " crate docs",
                " lifetimes",
                " block /* nested */ still ",
                " four"
            ]
        );
    }

    #[test]
    fn python() {
        let src = "#!/usr/bin/python\n# -*- coding: utf-8 -*-\nx = '#no' + \"#no\"  # yes\n\"\"\"\n# docstring\n\"\"\"\ny = 1  # type: ignore\ns = 'it\\'s # no'  # two\n";
        assert_eq!(texts("a.py", src), [" yes", " two"]);
    }

    #[test]
    fn c_and_cpp() {
        let src = "#include <a.h> // inc\nchar *s = \"/* no */ // no\"; char c = '\"'; /* yes */\nint n = 1'000; // NOLINT\n";
        assert_eq!(texts("a.c", src), [" inc", " yes "]);
        let cpp = "auto s = R\"(// raw \" /* no */)\"; // yes\n/// doc\n";
        assert_eq!(texts("a.cpp", cpp), [" yes", " doc"]);
    }

    #[test]
    fn csharp() {
        let src = "var a = @\"C:\\dir\\\"; // one\nvar b = \"\"\"\n// raw\n\"\"\"; /* two */\n/// <summary>doc</summary>\n";
        assert_eq!(
            texts("a.cs", src),
            [" one", " two ", " <summary>doc</summary>"]
        );
    }

    #[test]
    fn makefile() {
        let src = "# top\nCC = gcc # tail\nall:\n\t@echo \"# no\" $# $$# ${#x} a#b \\# no\n\techo done # yes\n";
        for file in [
            "Makefile",
            "makefile",
            "GNUmakefile",
            "rules.mk",
            "makefile.inc",
        ] {
            assert_eq!(texts(file, src), [" top", " tail", " yes"], "{file}");
        }
    }

    #[test]
    fn resource_and_include_files_use_c_rules() {
        let src = "#include \"a.h\" // one\nSTRINGTABLE { 1 \"a // \"\"no\"\"\" } /* two */\n";
        assert_eq!(texts("app.rc", src), [" one", " two "]);
        assert_eq!(texts("defs.inc", src), [" one", " two "]);
        assert_eq!(texts("defs.hxx", "int a; // one\n"), [" one"]);
    }

    #[test]
    fn typescript_wasm_plugin() {
        let src = r#"#!/usr/bin/env node
const url = "http://a" + 'http://b' + `http://c ${ `http://d ${x /* one */}` } // no`; // two
const re = /[/]\/"'`/g, half = a / 2 / b; /** three */
if (/^\/\//.test(s)) return /\//; // four
type T<A> = { a: A }; /**/ let s = `}${ {a: 1}.a }//no`;
"#;
        assert_eq!(texts("a.ts", src), [" one ", " two", " three ", " four"]);
    }

    #[test]
    fn lua_wasm_plugin() {
        let src = "#!/usr/bin/lua\nlocal s = \"-- no\" -- one\nlocal t = [==[\n-- no ]]\n]==] --[[ two\n]] x = 1\n--[=[ three ]] ]=]\nlocal u = '--[[' -- four";
        assert_eq!(
            texts("a.lua", src),
            [" one", " two\n", " three ]] ", " four"]
        );
    }
}
