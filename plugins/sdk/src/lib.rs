//! Glue for writing codecleanup language plugins in Rust.
//!
//! A plugin implements [`Language`] and calls [`export_plugin!`], which
//! provides everything version 1 of the plugin ABI asks for (see
//! `src/wasm.rs` of the host). The plugin only classifies bytes; it has no
//! access to files or anything else outside its own memory.

#![no_std]

pub const CODE: u32 = 0;
pub const LINE_OPEN: u32 = 1;
pub const BLOCK_OPEN: u32 = 2;
pub const TEXT: u32 = 3;
pub const CLOSE: u32 = 4;

/// Size of the window the host fills per call.
pub const INPUT_CAP: usize = 64 * 1024;
const MAX_SEGMENTS: usize = 4096;

/// The result of one scan: runs of bytes and what they are.
pub struct Segments {
    items: [[u32; 2]; MAX_SEGMENTS],
    count: usize,
}

impl Segments {
    /// Appends a run. Adjacent runs of code or of text are merged.
    pub fn push(&mut self, kind: u32, len: usize) {
        if len == 0 {
            return;
        }
        if self.count > 0 && (kind == CODE || kind == TEXT) && self.items[self.count - 1][0] == kind
        {
            self.items[self.count - 1][1] += len as u32;
        } else {
            self.items[self.count] = [kind, len as u32];
            self.count += 1;
        }
    }

    /// Whether three more runs fit. A scan has to return once this is
    /// false; the host calls again with the rest.
    pub fn has_room(&self) -> bool {
        self.count + 3 <= MAX_SEGMENTS
    }
}

pub trait Language {
    /// TOML with `name` and `extensions` and/or `filenames`.
    const INFO: &'static str;
    /// The state at the start of a file.
    const START: Self;

    /// Classifies a prefix of `input` into `out` and returns its length.
    ///
    /// The rest is handed in again with more data. With `eof` set, `input`
    /// is the end of the file and has to be consumed completely, unless
    /// `out` runs out of room.
    fn scan(&mut self, input: &[u8], eof: bool, out: &mut Segments) -> usize;
}

/// The state behind the exported functions.
pub struct Plugin<L> {
    pub input: [u8; INPUT_CAP],
    pub segments: Segments,
    language: L,
}

impl<L: Language> Plugin<L> {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Plugin {
            input: [0; INPUT_CAP],
            segments: Segments {
                items: [[0; 2]; MAX_SEGMENTS],
                count: 0,
            },
            language: L::START,
        }
    }

    pub fn reset(&mut self) {
        self.language = L::START;
        self.segments.count = 0;
    }

    pub fn scan(&mut self, len: i32, eof: i32) -> i32 {
        self.segments.count = 0;
        if len < 0 || len as usize > INPUT_CAP {
            return -1;
        }
        self.language
            .scan(&self.input[..len as usize], eof != 0, &mut self.segments) as i32
    }

    pub fn segments_ptr(&self) -> i32 {
        self.segments.items.as_ptr() as i32
    }

    pub fn segments_len(&self) -> i32 {
        self.segments.count as i32
    }
}

/// Exports a [`Language`] as a codecleanup plugin.
#[macro_export]
macro_rules! export_plugin {
    ($language:ty) => {
        static mut PLUGIN: $crate::Plugin<$language> = $crate::Plugin::new();

        fn plugin() -> &'static mut $crate::Plugin<$language> {
            // SAFETY: a WebAssembly instance is single-threaded and none of
            // the exports is reentrant, so this is the only reference.
            unsafe { &mut *::core::ptr::addr_of_mut!(PLUGIN) }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn cc_abi_version() -> i32 {
            1
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn cc_info_ptr() -> i32 {
            <$language as $crate::Language>::INFO.as_ptr() as i32
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn cc_info_len() -> i32 {
            <$language as $crate::Language>::INFO.len() as i32
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn cc_input_ptr() -> i32 {
            plugin().input.as_ptr() as i32
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn cc_input_cap() -> i32 {
            $crate::INPUT_CAP as i32
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn cc_reset() {
            plugin().reset()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn cc_scan(len: i32, eof: i32) -> i32 {
            plugin().scan(len, eof)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn cc_segments_ptr() -> i32 {
            plugin().segments_ptr()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn cc_segments_len() -> i32 {
            plugin().segments_len()
        }

        #[cfg(target_arch = "wasm32")]
        #[panic_handler]
        fn panic(_: &::core::panic::PanicInfo) -> ! {
            ::core::arch::wasm32::unreachable()
        }
    };
}
