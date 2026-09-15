//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! On-disk cache for compiled wasmer modules.
//!
//! Cranelift compilation of WASM templates is expensive: ~6 MB peak heap and
//! tens of milliseconds per template, paid on every node startup. The compiled
//! output for a given `(wasm_source, engine_config)` is deterministic and
//! reusable, so it can be persisted on local disk and loaded back via
//! [`wasmer::Module::deserialize_unchecked`] in milliseconds with negligible
//! peak heap.
//!
//! The WASM source bytes themselves stay on-chain (canonical, deterministic
//! representation). This cache is strictly node-local: any corrupt or missing
//! entry falls back to a full compile from source, with no consensus
//! implication.
//!
//! Two surfaces are exposed:
//!
//! - [`WasmModuleCache`] — low-level helper for callers that don't sit in a `TemplateProvider` chain (e.g. the wallet
//!   daemon's template monitor).
//! - [`DiskCachedWasmTemplateProvider`] — a `TemplateProvider` middleware that wraps a raw `PublishedTemplate` provider
//!   and outputs `LoadedTemplate`, doing compile-or-deserialize behind the scenes.

use std::{
    fs,
    io,
    path::{Path, PathBuf},
    sync::atomic::{self, AtomicU64},
};

use log::*;
use memmap2::Mmap;
use tari_engine_types::{limits::ModuleShape, published_template::PublishedTemplate};
use tari_ootle_common_types::{
    Epoch,
    services::template_provider::{TemplateMetadataProvider, TemplateProvider, TemplateProviderMetadata},
};
use tari_template_builtin::is_builtin_template_address;
use tari_template_lib::types::TemplateAddress;

use crate::{
    template::{LoadedTemplate, TemplateLoaderError},
    wasm::WasmModule,
};

const LOG_TARGET: &str = "tari::engine::wasm::cache";

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Engine-config fingerprint embedded in cache filenames.
///
/// Bump this string whenever any of the following change, otherwise nodes
/// loading from a stale cache will misbehave (deserialize failures at best,
/// undefined behaviour at worst):
///
/// - [`crate::wasm::WasmModule::create_engine`] config (compiler flags, features bitset, middleware list, tunables).
/// - [`tari_engine_types::limits::MAX_WASM_POINTS_PER_CALL`], which the metering middleware bakes into the artifact as
///   the initial remaining-points global. `WasmProcess::metering_allowance` reads it back with `get_remaining_points`,
///   so a node serving a stale artifact meters against the old cap and diverges from one that compiled fresh.
/// - The `wasmer` crate version (the serialized artifact format is internal to wasmer and not part of any stable wire
///   spec).
/// - The order or meaning of the header's fields at an unchanged `HEADER_BYTES`. A reader that agrees with the writer
///   on the header's length but not on its field order recomputes a matching CRC over values it then reads from the
///   wrong bytes. A change to the length needs no bump: it moves the artifact's start, so the body no longer begins
///   with wasmer's magic and `deserialize_unchecked` rejects the file. A header that grows is caught one step earlier,
///   at a CRC read from artifact bytes.
/// - How the header's values are derived — `validate_module_structure`'s segment tally. A cache hit serves these
///   verbatim rather than recomputing them, and `instantiation_points` prices a call off them, so a node reading a file
///   written under an older derivation charges a different fee for the same transaction than one that compiled fresh.
///   These are consensus values, not accounting hints.
///
/// On a bump, old cache files become orphans (different filename suffix)
/// and the next compile-from-source rewrites under the new key.
pub const ENGINE_FINGERPRINT: &str = "v5";

/// Five 8-byte LE fields at the head of each cache file: the original WASM source byte count
/// followed by the four counts of [`ModuleShape`]. `wasmer::Module::serialize` preserves none of
/// them, and all are needed after a cache hit — the first for accounting (e.g. moka weighing), the
/// rest to price instantiation.
const HEADER_FIELD_COUNT: usize = 5;
const HEADER_FIELD_BYTES: usize = HEADER_FIELD_COUNT * 8;

/// Zero padding between the fields and the CRC. This is the knob that satisfies the alignment
/// assert below when the field count changes; the CRC stays a `u64`.
const HEADER_PAD_BYTES: usize = 0;

/// Offset of the 8-byte LE CRC32 that covers every header byte before it.
///
/// The fields sit outside the wasmer artifact, so `deserialize_unchecked` has no view of damage
/// confined to them, while the four shape counts price `instantiation_points` into a committed fee
/// receipt. The CRC is the only check that reaches those bytes.
const CRC_OFFSET: usize = HEADER_FIELD_BYTES + HEADER_PAD_BYTES;

/// The header fields, the pad and the CRC.
///
/// The total is a multiple of 16: the artifact starts at `HEADER_BYTES` into a page-aligned mmap
/// and its rkyv metadata a further 32 bytes in, where `rkyv::access_unchecked` reads an archived
/// root that wasmer aligns to `MetadataHeader::ALIGN` (16); `MetadataHeader::parse` enforces the
/// weaker 8-byte bound on the artifact itself.
const HEADER_BYTES: usize = CRC_OFFSET + 8;

const _: () = assert!(
    HEADER_BYTES.is_multiple_of(16),
    "the artifact's rkyv metadata must start 16-byte aligned: widen HEADER_PAD_BYTES",
);

/// Low-level on-disk cache for compiled wasmer modules.
///
/// Files live at `{dir}/{template_address}_{ENGINE_FINGERPRINT}.bin`.
/// The body is `[u64 LE: code_size][u64 LE x4: ModuleShape][u64 LE: CRC32 of the preceding fields]
/// || wasmer::Module::serialize(...)`.
///
/// Writes are atomic (tempfile + rename). Read failures (missing file,
/// deserialize errors, format changes) are non-fatal: the corrupt file is
/// removed and the caller is expected to recompile from source.
#[derive(Debug, Clone)]
pub struct WasmModuleCache {
    dir: PathBuf,
}

impl WasmModuleCache {
    /// Open or create a cache rooted at `dir`. Creates the directory tree
    /// if missing.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, addr: &TemplateAddress) -> PathBuf {
        self.dir.join(format!("{}_{}.bin", addr, ENGINE_FINGERPRINT))
    }

    /// Try to load a previously-cached module for `addr`. Returns `None` on
    /// any miss — file missing, header malformed, deserialize failure. On
    /// recoverable corruption the bad file is removed so a subsequent `store`
    /// can replace it.
    ///
    /// The file is `mmap`'d rather than read into a `Vec<u8>` — wasmer's
    /// deserialize path accepts `bytes::Bytes` and `Bytes::from_owner` lets us
    /// hand it the mmap region without copying. Cache hits cost a single
    /// `mmap` syscall (and the page faults wasmer's deserializer triggers as
    /// it walks the artifact); no full-artifact allocation.
    pub fn try_load(&self, addr: &TemplateAddress) -> Option<LoadedTemplate> {
        let path = self.path_for(addr);
        let file = match fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
            Err(e) => {
                warn!(
                    target: LOG_TARGET,
                    "Failed to open cache file {}: {}", path.display(), e,
                );
                return None;
            },
        };

        // SAFETY: see the docs on `Mmap::map`. A mapping SIGBUSes if the file shrinks under it, so
        // what keeps this sound is that `store` never writes through the published path: it fills a
        // tempfile only that call can name and renames, pointing the directory entry at a new inode
        // and leaving this mapping's inode whole. Writers under a different engine config target a
        // different filename through the fingerprint suffix.
        let mmap = match unsafe { Mmap::map(&file) } {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    target: LOG_TARGET,
                    "Failed to mmap cache file {}: {}", path.display(), e,
                );
                // An empty file cannot be mapped at all, so it never reaches the length check below.
                // Other map failures are resource limits, under which the file may still be good.
                if file.metadata().is_ok_and(|m| m.len() == 0) {
                    let _ignore = fs::remove_file(&path);
                }
                return None;
            },
        };

        if mmap.len() < HEADER_BYTES {
            warn!(
                target: LOG_TARGET,
                "Cache file {} is shorter than the {}-byte header; removing.",
                path.display(),
                HEADER_BYTES,
            );
            drop(mmap);
            let _ignore = fs::remove_file(&path);
            return None;
        }

        let stored_crc = u64::from_le_bytes(
            mmap[CRC_OFFSET..HEADER_BYTES]
                .try_into()
                .expect("HEADER_BYTES - CRC_OFFSET is 8"),
        );
        let computed_crc = u64::from(crc32fast::hash(&mmap[..CRC_OFFSET]));
        if stored_crc != computed_crc {
            warn!(
                target: LOG_TARGET,
                "Cache file {} has a corrupt header (CRC {:#010x}, expected {:#010x}); removing.",
                path.display(),
                stored_crc,
                computed_crc,
            );
            drop(mmap);
            let _ignore = fs::remove_file(&path);
            return None;
        }

        let mut field = [0u8; 8];
        let mut read_field = |i: usize| {
            field.copy_from_slice(&mmap[i * 8..(i + 1) * 8]);
            u64::from_le_bytes(field)
        };
        let code_size = read_field(0) as usize;
        let shape = ModuleShape {
            data_segment_bytes: read_field(1),
            data_segment_count: read_field(2),
            element_segment_entries: read_field(3),
            declared_table_slots: read_field(4),
        };

        // Wrap the mmap as a Bytes that owns it, then slice past the
        // header. `Bytes::slice` is zero-copy (pointer + length
        // adjustment); the wrapped Mmap is dropped only when the resulting
        // Bytes (and any clones the deserializer may keep) goes out of
        // scope.
        let body = bytes::Bytes::from_owner(mmap).slice(HEADER_BYTES..);

        // SAFETY: bytes were written by [`Self::store`] in a previous run of
        // this process (or an earlier process owning the same data dir) via
        // `wasmer::Module::serialize`. The cache directory is node-local and
        // not attacker-controlled in any sane operational setup. The
        // fingerprint suffix in the filename guarantees the engine config
        // matches this build; a deserialize failure simply triggers the
        // recompile fallback.
        match unsafe { WasmModule::load_template_from_serialized(body, code_size, shape) } {
            Ok(loaded) => {
                debug!(target: LOG_TARGET, "Cache hit for template {}", addr);
                Some(loaded)
            },
            Err(err) => {
                warn!(
                    target: LOG_TARGET,
                    "Failed to deserialize cached module {}: {}; removing.",
                    path.display(),
                    err,
                );
                let _ignore = fs::remove_file(&path);
                None
            },
        }
    }

    /// Persist a compiled module under `addr`. Best-effort: on any failure
    /// (serialize, write, rename) a warning is logged and the call returns
    /// successfully — the caller's compiled module is still valid.
    pub fn store(&self, addr: &TemplateAddress, loaded: &LoadedTemplate) {
        let LoadedTemplate::Wasm(wasm) = loaded;
        let serialized = match wasm.wasm_module().serialize() {
            Ok(s) => s,
            Err(e) => {
                warn!(target: LOG_TARGET, "Failed to serialize module for {}: {}", addr, e);
                return;
            },
        };

        let path = self.path_for(addr);
        // Every call needs its own tempfile: two `store`s of one address can run concurrently,
        // since the indexer opens the cache directory twice, once for the template manager and
        // once for the dry-run provider.
        let tmp = self.dir.join(format!(
            "{}_{}.bin.tmp.{}.{}",
            addr,
            ENGINE_FINGERPRINT,
            std::process::id(),
            TMP_COUNTER.fetch_add(1, atomic::Ordering::Relaxed),
        ));

        let shape = wasm.shape();
        // `HEADER_FIELD_COUNT` fields, in the order `try_load` reads them.
        let fields: [u64; HEADER_FIELD_COUNT] = [
            wasm.code_size() as u64,
            shape.data_segment_bytes,
            shape.data_segment_count,
            shape.element_segment_entries,
            shape.declared_table_slots,
        ];
        let mut header = [0u8; HEADER_BYTES];
        for (slot, value) in header.chunks_exact_mut(8).zip(fields) {
            slot.copy_from_slice(&value.to_le_bytes());
        }
        let crc = u64::from(crc32fast::hash(&header[..CRC_OFFSET]));
        header[CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());

        let mut bytes = Vec::with_capacity(HEADER_BYTES + serialized.len());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&serialized);

        if let Err(e) = write_durable(&tmp, &bytes) {
            warn!(target: LOG_TARGET, "Failed to write cache tempfile {}: {}", tmp.display(), e);
            let _ignore = fs::remove_file(&tmp);
            return;
        }

        if let Err(e) = fs::rename(&tmp, &path) {
            warn!(
                target: LOG_TARGET,
                "Failed to rename {} -> {}: {}", tmp.display(), path.display(), e,
            );
            let _ignore = fs::remove_file(&tmp);
            return;
        }

        debug!(
            target: LOG_TARGET,
            "Cached compiled module for template {} -> {}", addr, path.display(),
        );
    }
}

/// Write `bytes` to `path`, flushed to the device before returning.
///
/// [`WasmModuleCache::store`] publishes a file by rename, which can expose contents still held in
/// the page cache. The artifact body lies past the header CRC's coverage, so it must reach the
/// device before the rename names it. Durability stops at the contents: a rename lost to a crash
/// costs one recompile.
fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// `TemplateProvider` middleware that adds an on-disk compiled-module cache
/// behind any provider returning raw [`PublishedTemplate`] bytes.
///
/// On `get_template(addr)`:
/// 1. If the cache file `{addr}_{ENGINE_FINGERPRINT}.bin` exists, deserialize and return — no compile, no
///    inner-provider call.
/// 2. Otherwise delegate to `inner` for the raw `PublishedTemplate`, compile via
///    [`WasmModule::load_template_from_code`], persist the compiled module to the cache, return.
///
/// Intended placement is between an outer in-memory cache (e.g. moka) and the
/// raw state-store provider, so a process-lifetime hot path skips disk
/// entirely and only the first compile-then-deserialize crossing per template
/// per node ever pays the disk cost.
#[derive(Debug, Clone)]
pub struct DiskCachedWasmTemplateProvider<TStore> {
    inner: TStore,
    cache: WasmModuleCache,
}

impl<TStore> DiskCachedWasmTemplateProvider<TStore> {
    pub fn new(inner: TStore, cache: WasmModuleCache) -> Self {
        Self { inner, cache }
    }

    pub fn open(inner: TStore, path: impl Into<PathBuf>) -> io::Result<Self> {
        let wasm_cache = WasmModuleCache::open(path)?;
        Ok(Self::new(inner, wasm_cache))
    }
}

impl<TStore> TemplateProvider for DiskCachedWasmTemplateProvider<TStore>
where TStore: TemplateProvider<Template = PublishedTemplate> + Clone + 'static
{
    type Error = DiskCachedWasmTemplateProviderError;
    type Template = LoadedTemplate;

    fn get_template(&self, address: &TemplateAddress) -> Result<Option<Self::Template>, Self::Error> {
        // Builtins bypass the disk cache: their addresses are hardcoded
        // constants (independent of binary content), so a cache entry under
        // a builtin's address would silently serve an out-of-date compiled
        // module after a builtin recompile. User-template addresses are
        // content-addressed, so binary changes implicitly invalidate the
        // cache key.
        if is_builtin_template_address(address) {
            let Some(published) = self
                .inner
                .get_template(address)
                .map_err(|e| DiskCachedWasmTemplateProviderError::Inner(e.into()))?
            else {
                return Ok(None);
            };
            return Ok(Some(WasmModule::load_template_from_code(published.binary.as_slice())?));
        }

        if let Some(loaded) = self.cache.try_load(address) {
            return Ok(Some(loaded));
        }

        let Some(published) = self
            .inner
            .get_template(address)
            .map_err(|e| DiskCachedWasmTemplateProviderError::Inner(e.into()))?
        else {
            return Ok(None);
        };

        let loaded = WasmModule::load_template_from_code(published.binary.as_slice())?;
        self.cache.store(address, &loaded);
        Ok(Some(loaded))
    }

    fn has_template(&self, address: &TemplateAddress) -> Result<bool, Self::Error> {
        // Cheap path: cache hit implies the template exists. A miss falls
        // through to the inner provider, which is allowed to answer without
        // materialising the binary.
        if !is_builtin_template_address(address) && self.cache.path_for(address).exists() {
            return Ok(true);
        }
        self.inner
            .has_template(address)
            .map_err(|e| DiskCachedWasmTemplateProviderError::Inner(e.into()))
    }
}

impl<TStore> TemplateMetadataProvider for DiskCachedWasmTemplateProvider<TStore>
where TStore: TemplateProvider<Template = PublishedTemplate> + Clone + 'static
{
    fn get_template_metadata(&self, id: &TemplateAddress) -> Result<Option<TemplateProviderMetadata>, Self::Error> {
        // Metadata always reads from the underlying state store, never from
        // the disk cache (the cache only stores the compiled module, not the
        // PublishedTemplate's author / epoch / metadata_hash fields).
        let template = self
            .inner
            .get_template(id)
            .map_err(|e| DiskCachedWasmTemplateProviderError::Inner(e.into()))?;
        Ok(template.map(|t| TemplateProviderMetadata {
            author: t.author,
            binary_hash: t.to_binary_hash(),
            epoch: Epoch(t.at_epoch),
            metadata_hash: t.metadata_hash,
        }))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DiskCachedWasmTemplateProviderError {
    #[error("Inner template provider error: {0}")]
    Inner(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    TemplateLoader(#[from] TemplateLoaderError),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tari_engine_types::published_template::PublishedTemplate;
    use tari_template_builtin::all_builtin_templates;
    use tari_template_lib::types::crypto::RistrettoPublicKeyBytes;
    use tempfile::TempDir;

    use super::*;

    #[derive(Clone)]
    struct StaticStore {
        templates: Arc<std::collections::HashMap<TemplateAddress, PublishedTemplate>>,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("not found")]
    struct StaticStoreError;

    impl TemplateProvider for StaticStore {
        type Error = StaticStoreError;
        type Template = PublishedTemplate;

        fn get_template(&self, address: &TemplateAddress) -> Result<Option<Self::Template>, Self::Error> {
            Ok(self.templates.get(address).cloned())
        }
    }

    fn make_store() -> (StaticStore, TemplateAddress) {
        // We re-use the Account builtin's *binary* (it's a real, valid WASM
        // template available in dev-deps) but file it under a synthetic
        // non-builtin address — the disk-cache path bypasses real builtin
        // addresses by design (see is_builtin_template_address).
        let template = all_builtin_templates()
            .iter()
            .find(|t| t.name == "Account")
            .expect("Account builtin");
        let test_addr = TemplateAddress::from_array([
            0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42,
            0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42,
        ]);
        debug_assert!(
            !is_builtin_template_address(&test_addr),
            "test address must not collide with a builtin",
        );
        let mut map = std::collections::HashMap::new();
        let published = PublishedTemplate {
            template_name: template.name.try_into().expect("valid name"),
            author: RistrettoPublicKeyBytes::default(),
            binary: template.binary.to_vec().try_into().expect("template binary too large"),
            at_epoch: 0,
            metadata_hash: None,
        };
        map.insert(test_addr, published);
        (
            StaticStore {
                templates: Arc::new(map),
            },
            test_addr,
        )
    }

    #[test]
    fn round_trip_compile_then_deserialize() {
        let dir = TempDir::new().unwrap();
        let cache = WasmModuleCache::open(dir.path()).unwrap();
        let (store, addr) = make_store();
        let provider = DiskCachedWasmTemplateProvider::new(store.clone(), cache.clone());

        // First call: cache miss, compile-then-store.
        let first = provider.get_template(&addr).unwrap().expect("loaded");
        assert!(cache.path_for(&addr).exists(), "store should write a file");

        // Second call: cache hit, deserialize-only path.
        let second = provider.get_template(&addr).unwrap().expect("loaded");
        assert_eq!(first.template_name(), second.template_name());
        assert_eq!(first.code_size(), second.code_size());
        assert_eq!(
            first.template_def().functions().len(),
            second.template_def().functions().len(),
        );
    }

    #[test]
    fn corrupt_cache_falls_back_to_recompile() {
        let dir = TempDir::new().unwrap();
        let cache = WasmModuleCache::open(dir.path()).unwrap();
        let (store, addr) = make_store();

        // Plant garbage at the expected filename.
        let path = cache.path_for(&addr);
        fs::write(&path, b"this is not a wasmer artifact").unwrap();
        assert!(path.exists());

        // try_load should return None, having removed the corrupt file.
        assert!(cache.try_load(&addr).is_none());
        assert!(!path.exists(), "corrupt file should be removed");

        // Provider compiles fresh and writes a valid file.
        let provider = DiskCachedWasmTemplateProvider::new(store, cache.clone());
        provider.get_template(&addr).unwrap().expect("loaded");
        assert!(path.exists(), "fresh compile should re-populate the cache");

        // And the freshly-cached file deserializes cleanly.
        assert!(cache.try_load(&addr).is_some());
    }

    #[test]
    fn flipped_header_byte_falls_back_to_recompile() {
        let dir = TempDir::new().unwrap();
        let cache = WasmModuleCache::open(dir.path()).unwrap();
        let (store, addr) = make_store();
        let provider = DiskCachedWasmTemplateProvider::new(store, cache.clone());
        provider.get_template(&addr).unwrap().expect("loaded");

        // Damage a shape count, leaving the wasmer artifact itself intact.
        let path = cache.path_for(&addr);
        let mut bytes = fs::read(&path).unwrap();
        bytes[8] ^= 0x01;
        fs::write(&path, &bytes).unwrap();

        assert!(cache.try_load(&addr).is_none(), "a bad header CRC is a miss");
        assert!(!path.exists(), "the file with the bad CRC should be removed");
    }

    #[test]
    fn flipped_crc_byte_falls_back_to_recompile() {
        let dir = TempDir::new().unwrap();
        let cache = WasmModuleCache::open(dir.path()).unwrap();
        let (store, addr) = make_store();
        let provider = DiskCachedWasmTemplateProvider::new(store, cache.clone());
        provider.get_template(&addr).unwrap().expect("loaded");

        let path = cache.path_for(&addr);
        let mut bytes = fs::read(&path).unwrap();
        bytes[CRC_OFFSET] ^= 0x01;
        fs::write(&path, &bytes).unwrap();

        assert!(cache.try_load(&addr).is_none());
        assert!(!path.exists());
    }

    #[test]
    fn truncated_header_treated_as_miss() {
        let dir = TempDir::new().unwrap();
        let cache = WasmModuleCache::open(dir.path()).unwrap();
        let (_store, addr) = make_store();

        let path = cache.path_for(&addr);
        fs::write(&path, vec![0u8; HEADER_BYTES - 1]).unwrap();

        assert!(cache.try_load(&addr).is_none());
        assert!(!path.exists(), "a short file should be removed");
    }

    #[test]
    fn stored_header_crc_covers_the_fields() {
        let dir = TempDir::new().unwrap();
        let cache = WasmModuleCache::open(dir.path()).unwrap();
        let (store, addr) = make_store();
        let provider = DiskCachedWasmTemplateProvider::new(store, cache.clone());
        let loaded = provider.get_template(&addr).unwrap().expect("loaded");

        let bytes = fs::read(cache.path_for(&addr)).unwrap();
        let stored_crc = u64::from_le_bytes(bytes[CRC_OFFSET..HEADER_BYTES].try_into().unwrap());
        assert_eq!(stored_crc, u64::from(crc32fast::hash(&bytes[..CRC_OFFSET])));

        let reloaded = cache.try_load(&addr).expect("hit");
        assert_eq!(reloaded.code_size(), loaded.code_size());

        // `instantiation_points` prices each shape count with its own constant, so a hit must serve
        // them in the slots a fresh compile wrote them to.
        let LoadedTemplate::Wasm(reloaded) = &reloaded;
        let LoadedTemplate::Wasm(loaded) = &loaded;
        assert_eq!(reloaded.shape(), loaded.shape());
    }

    #[test]
    fn fingerprint_mismatch_treated_as_miss() {
        let dir = TempDir::new().unwrap();
        let cache = WasmModuleCache::open(dir.path()).unwrap();
        let (_store, addr) = make_store();

        // Plant a file under a different fingerprint suffix.
        let alt = dir.path().join(format!("{}_v0.bin", addr));
        fs::write(&alt, b"some bytes").unwrap();

        // Real path doesn't exist; try_load returns None and doesn't touch alt.
        assert!(cache.try_load(&addr).is_none());
        assert!(alt.exists(), "files for other fingerprints are left alone");
    }
}
