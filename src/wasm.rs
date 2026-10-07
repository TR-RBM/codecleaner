//! WebAssembly language plugins, for syntax the TOML rules cannot express.
//!
//! A plugin is a module without imports, so it cannot reach files, the
//! network or anything else on the host. It exports (ABI version 1):
//!
//! | export                           | meaning                                    |
//! |----------------------------------|--------------------------------------------|
//! | `memory`                         | the linear memory                          |
//! | `cc_abi_version() -> i32`        | `1`                                        |
//! | `cc_info_ptr/cc_info_len -> i32` | TOML text: `name`, `extensions`, `filenames` |
//! | `cc_input_ptr/cc_input_cap`      | buffer the host copies source bytes into   |
//! | `cc_reset()`                     | start a new file                           |
//! | `cc_scan(len, eof) -> i32`       | classify the buffer, return bytes consumed |
//! | `cc_segments_ptr/cc_segments_len`| result: `len` pairs of `u32` (kind, length)|
//!
//! `cc_scan` follows the contract of [`Lexer::scan`]; the kinds are the
//! values of [`Kind`].

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use wasmi::{Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

use crate::lexer::{Kind, Lexer, Segment};

const ABI_VERSION: i32 = 1;

/// What a plugin says about itself.
#[derive(Debug, Deserialize)]
pub struct Info {
    pub name: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub filenames: Vec<String>,
}

pub struct WasmLexer {
    store: Store<()>,
    memory: Memory,
    reset: TypedFunc<(), ()>,
    scan: TypedFunc<(i32, i32), i32>,
    segments_ptr: TypedFunc<(), i32>,
    segments_len: TypedFunc<(), i32>,
    input_ptr: usize,
    input_cap: usize,
}

/// Instantiates a plugin and reads its description.
pub fn load(wasm: &[u8]) -> Result<(Info, WasmLexer)> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm).map_err(|err| anyhow!("invalid module: {err}"))?;
    let mut store = Store::new(&engine, ());
    let instance = Linker::<()>::new(&engine)
        .instantiate_and_start(&mut store, &module)
        .map_err(|err| anyhow!("cannot instantiate (plugins must not import anything): {err}"))?;
    let memory = instance
        .get_memory(&store, "memory")
        .context("no `memory` export")?;

    let version = read(&instance, &mut store, "cc_abi_version")?;
    ensure!(
        version == ABI_VERSION,
        "ABI version {version} is not supported, expected {ABI_VERSION}"
    );
    let info_ptr = read(&instance, &mut store, "cc_info_ptr")? as u32 as usize;
    let info_len = read(&instance, &mut store, "cc_info_len")? as u32 as usize;
    let info = memory
        .data(&store)
        .get(info_ptr..info_ptr.saturating_add(info_len))
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .context("the plugin info is not readable text")?;
    let info: Info = toml::from_str(info).context("the plugin info is not valid")?;

    let input_ptr = read(&instance, &mut store, "cc_input_ptr")? as u32 as usize;
    let input_cap = read(&instance, &mut store, "cc_input_cap")? as u32 as usize;
    ensure!(
        input_cap > 0 && input_ptr.saturating_add(input_cap) <= memory.data(&store).len(),
        "the input buffer is outside of the plugin memory"
    );
    let lexer = WasmLexer {
        reset: export(&instance, &store, "cc_reset")?,
        scan: export(&instance, &store, "cc_scan")?,
        segments_ptr: getter(&instance, &store, "cc_segments_ptr")?,
        segments_len: getter(&instance, &store, "cc_segments_len")?,
        store,
        memory,
        input_ptr,
        input_cap,
    };
    Ok((info, lexer))
}

fn export<P: wasmi::WasmParams, R: wasmi::WasmResults>(
    instance: &Instance,
    store: &Store<()>,
    name: &str,
) -> Result<TypedFunc<P, R>> {
    instance
        .get_typed_func(store, name)
        .map_err(|err| anyhow!("missing or mistyped export `{name}`: {err}"))
}

fn getter(instance: &Instance, store: &Store<()>, name: &str) -> Result<TypedFunc<(), i32>> {
    export(instance, store, name)
}

/// Calls an export that takes nothing and returns a number.
fn read(instance: &Instance, store: &mut Store<()>, name: &str) -> Result<i32> {
    let func = getter(instance, store, name)?;
    call(store, &func)
}

fn call(store: &mut Store<()>, func: &TypedFunc<(), i32>) -> Result<i32> {
    func.call(store, ())
        .map_err(|err| anyhow!("the plugin failed: {err}"))
}

impl Lexer for WasmLexer {
    fn reset(&mut self) {
        // A plugin that traps here will report it on the next scan.
        let _ = self.reset.call(&mut self.store, ());
    }

    fn scan(&mut self, input: &[u8], eof: bool, out: &mut Vec<Segment>) -> Result<usize> {
        let len = input.len().min(self.input_cap);
        let eof = eof && len == input.len();
        let buffer = self
            .memory
            .data_mut(&mut self.store)
            .get_mut(self.input_ptr..self.input_ptr + len)
            .context("the plugin shrank its memory")?;
        buffer.copy_from_slice(&input[..len]);
        let used = self
            .scan
            .call(&mut self.store, (len as i32, eof as i32))
            .map_err(|err| anyhow!("the plugin failed: {err}"))?;
        ensure!(
            used >= 0 && used as usize <= len,
            "the plugin reported error {used}"
        );
        if used == 0 && len == self.input_cap {
            bail!("the plugin cannot make progress with its own buffer size");
        }
        let ptr = call(&mut self.store, &self.segments_ptr)? as u32 as usize;
        let count = call(&mut self.store, &self.segments_len)? as u32 as usize;
        let table = self
            .memory
            .data(&self.store)
            .get(ptr..ptr.saturating_add(count.saturating_mul(8)))
            .context("the plugin returned segments outside of its memory")?;
        for pair in table.chunks_exact(8) {
            let kind = u32::from_le_bytes([pair[0], pair[1], pair[2], pair[3]]);
            let len = u32::from_le_bytes([pair[4], pair[5], pair[6], pair[7]]);
            let kind =
                Kind::from_u32(kind).context("the plugin returned an unknown segment kind")?;
            out.push(Segment {
                kind,
                len: len as usize,
            });
        }
        Ok(used as usize)
    }
}
