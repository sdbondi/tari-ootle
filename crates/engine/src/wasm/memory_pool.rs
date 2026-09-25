//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! A pool of guest linear-memory mappings, reused across instantiations.
//!
//! Every static-style wasmer memory reserves the same ~8 GiB of address space (the 4 GiB wasm32
//! space plus the offset guard) and makes only the module's minimum accessible. Creating and
//! destroying that reservation per instantiation costs an `mmap`/`munmap` pair that serializes
//! every executing thread on the process's mmap lock and broadcasts TLB shootdowns on teardown —
//! measured as the ceiling on concurrent transaction execution. This pool keeps the reservations
//! alive and hands them out per instantiation, so the steady-state cost per guest memory is one
//! fixed re-map over the few pages the guest actually touched.
//!
//! # Guest-observable equivalence (consensus-critical)
//!
//! A pooled memory must be indistinguishable from a freshly mapped one:
//!
//! * **Contents**: on release the accessible range is replaced with a fresh anonymous mapping (`mmap` with
//!   `MAP_FIXED`), so the next tenant reads kernel-guaranteed zero pages, exactly like a fresh reservation. A fixed
//!   re-map is used rather than `madvise(MADV_DONTNEED)` because the latter's zero-fill guarantee is Linux-specific.
//! * **Protection**: cranelift compiles static-style memories without explicit bounds checks — an out-of-bounds access
//!   traps only because pages beyond `current_length` are `PROT_NONE`. The same re-map installs the range `PROT_NONE`,
//!   so the next tenant's protection boundary sits exactly at its own accessible size, never at a previous tenant's
//!   high-water mark.
//!
//! If the re-map fails the mapping is dropped (munmapped) instead of pooled, so a failure degrades
//! to the old per-instantiation cost, never to a semantic difference.

#[cfg(not(unix))]
compile_error!(
    "Windows is not supported: the tari engine manages guest memory with the mmap and mprotect system calls, which \
     only exist on unix systems (Linux, macOS)."
);

use std::{
    cell::UnsafeCell,
    ptr::NonNull,
    sync::{Arc, Mutex},
};

use wasmer::{
    MemoryError,
    MemoryStyle,
    MemoryType,
    Pages,
    TableStyle,
    TableType,
    sys::{
        Tunables,
        vm::{
            LinearMemory,
            MaybeInstanceOwned,
            Mmap,
            MmapType,
            VMMemory,
            VMMemoryDefinition,
            VMOwnedMemory,
            VMSharedMemory,
            VMTable,
            VMTableDefinition,
        },
    },
};

/// The uniform reservation size of a static-style memory: the full wasm32 address space plus the
/// offset guard. Computed from wasmer's own constants so a wasmer upgrade that changes the layout
/// changes the pool with it.
fn static_mapping_bytes() -> usize {
    let bound = MemoryStyle::static_bound().bytes().0;
    let guard = usize::try_from(MemoryStyle::Static.offset_guard_size()).expect("guard size exceeds usize");
    bound.checked_add(guard).expect("static mapping size overflows usize")
}

/// A pool of identical `PROT_NONE` address-space reservations for static-style guest memories.
///
/// Every pooled mapping is fully inaccessible and zero-filled (see the module docs), so acquiring
/// one is equivalent to a fresh `Mmap::accessible_reserved(0, ..)`.
#[derive(Debug)]
pub struct MemoryPool {
    mapping_bytes: usize,
    slots: Mutex<Vec<Mmap>>,
    max_slots: usize,
}

impl MemoryPool {
    pub fn new(max_slots: usize) -> Self {
        Self {
            mapping_bytes: static_mapping_bytes(),
            slots: Mutex::new(Vec::new()),
            max_slots,
        }
    }

    /// Number of reservations currently parked in the pool.
    pub fn pooled_count(&self) -> usize {
        self.slots.lock().unwrap().len()
    }

    fn acquire(&self) -> Result<Mmap, MemoryError> {
        if let Some(mmap) = self.slots.lock().unwrap().pop() {
            return Ok(mmap);
        }
        Mmap::accessible_reserved(0, self.mapping_bytes, None, MmapType::Private).map_err(MemoryError::Region)
    }

    /// Returns a mapping to the pool after scrubbing it back to the fresh-reservation state:
    /// `PROT_NONE` and zero-filled over the previously accessible range. A mapping that cannot be
    /// scrubbed, or that arrives while the pool is full, is dropped (munmapped) instead.
    fn release(&self, mut mmap: Mmap, accessible_bytes: usize) {
        if scrub(&mut mmap, accessible_bytes).is_err() {
            return;
        }
        let mut slots = self.slots.lock().unwrap();
        if slots.len() < self.max_slots {
            slots.push(mmap);
        }
    }
}

/// Zero-fills `mmap[..accessible_bytes]` and leaves `mmap[..minimum_bytes]` accessible and the rest
/// `PROT_NONE`. `accessible_bytes` is never below `minimum_bytes`: a memory never shrinks below the
/// module's minimum.
///
/// On Linux `MADV_DONTNEED` drops the pages of a private anonymous mapping, so each reads as zero
/// when next touched. It takes the process's mmap lock only for reading, so concurrent executions
/// do not serialize on it, and the protection change the lock must be taken for writing to make is
/// needed only when the memory grew past its minimum.
#[cfg(target_os = "linux")]
fn discard_contents(mmap: &mut Mmap, minimum_bytes: usize, accessible_bytes: usize) -> Result<(), MemoryError> {
    let base = mmap.as_mut_ptr();
    // SAFETY: both ranges lie within the mapping (accessible never exceeds mapping_bytes), and the
    // caller holds the only memory backed by it, which runs no guest code across the call.
    unsafe {
        if accessible_bytes > 0 && libc::madvise(base.cast(), accessible_bytes, libc::MADV_DONTNEED) != 0 {
            return Err(os_error("discard a guest memory's pages"));
        }
        if accessible_bytes > minimum_bytes &&
            libc::mprotect(
                base.add(minimum_bytes).cast(),
                accessible_bytes - minimum_bytes,
                libc::PROT_NONE,
            ) != 0
        {
            return Err(os_error("re-protect a guest memory's grown pages"));
        }
    }
    Ok(())
}

/// Zero-fills `mmap[..accessible_bytes]` and leaves `mmap[..minimum_bytes]` accessible and the rest
/// `PROT_NONE`. Outside Linux `MADV_DONTNEED` does not guarantee zero-filled pages, so the range is
/// re-mapped fresh instead.
#[cfg(not(target_os = "linux"))]
fn discard_contents(mmap: &mut Mmap, minimum_bytes: usize, accessible_bytes: usize) -> Result<(), MemoryError> {
    scrub(mmap, accessible_bytes)?;
    if minimum_bytes > 0 {
        mmap.make_accessible(0, minimum_bytes).map_err(MemoryError::Region)?;
    }
    Ok(())
}

fn os_error(action: &str) -> MemoryError {
    MemoryError::Generic(format!("could not {action}: {}", std::io::Error::last_os_error()))
}

/// Replaces `mmap[..accessible_bytes]` with a fresh zero-filled `PROT_NONE` anonymous mapping,
/// leaving the rest of the reservation untouched. The range is then exactly as a fresh
/// `Mmap::accessible_reserved(0, ..)` left it.
///
/// Nothing may reference the range's contents across this call: the caller either owns the mapping
/// outright or holds the only memory backed by it.
fn scrub(mmap: &mut Mmap, accessible_bytes: usize) -> Result<(), MemoryError> {
    if accessible_bytes == 0 {
        return Ok(());
    }
    let base = mmap.as_mut_ptr();
    // SAFETY: `base..base+accessible_bytes` lies within the mapping (accessible never exceeds
    // mapping_bytes), and the caller guarantees nothing reads it across the re-map.
    let remapped = unsafe {
        libc::mmap(
            base.cast(),
            accessible_bytes,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
            -1,
            0,
        )
    };
    if remapped == base.cast() {
        Ok(())
    } else {
        Err(os_error("re-map a guest memory"))
    }
}

/// A static-style guest linear memory backed by a pooled reservation.
///
/// Mirrors the semantics of wasmer's `VMOwnedMemory` for static memories — same definition
/// handling, same grow behaviour, same protection boundary — with two differences: the backing
/// mapping returns to the [`MemoryPool`] on drop instead of being unmapped, and
/// [`LinearMemory::reset`] returns the memory to the state a fresh instantiation creates rather
/// than only zeroing its size, so an instance can be restored to its freshly instantiated state.
#[derive(Debug)]
struct PooledLinearMemory {
    /// `None` only transiently during drop, when the mapping is handed back to the pool.
    mmap: Option<Mmap>,
    /// Bytes from the base that are `PROT_READ|PROT_WRITE`. Always equal to the definition's
    /// `current_length` (grow raises both together, reset returns both to the minimum), preserving the
    /// trap-at-current-length protection boundary the compiled code relies on.
    accessible_bytes: usize,
    size: Pages,
    memory_type: MemoryType,
    maximum: Pages,
    vm_memory_definition: MaybeInstanceOwned<VMMemoryDefinition>,
    pool: Arc<MemoryPool>,
}

// SAFETY: the raw definition pointer is owned by the instance this memory belongs to and lives
// exactly as long as it; wasmer's own VMOwnedMemory carries the same pointer with the same
// justification.
unsafe impl Send for PooledLinearMemory {}
unsafe impl Sync for PooledLinearMemory {}

impl PooledLinearMemory {
    /// # Safety
    /// `vm_memory_location`, when given, must point to a valid `VMMemoryDefinition` that outlives
    /// the returned memory (it is the instance's own definition slot).
    unsafe fn new(
        pool: Arc<MemoryPool>,
        ty: &MemoryType,
        vm_memory_location: Option<NonNull<VMMemoryDefinition>>,
    ) -> Result<Self, MemoryError> {
        // The tunables wrapping this pool guarantee a maximum by construction (LimitingTunables
        // rejects modules without one); a missing maximum here is a wiring bug.
        let maximum = ty
            .maximum
            .ok_or_else(|| MemoryError::Generic("pooled memory requires a declared maximum".to_string()))?;
        if maximum < ty.minimum {
            return Err(MemoryError::InvalidMemory {
                reason: format!(
                    "the maximum ({} pages) is less than the minimum ({} pages)",
                    maximum.0, ty.minimum.0
                ),
            });
        }
        if maximum > Pages::max_value() {
            return Err(MemoryError::MaximumMemoryTooLarge {
                max_requested: maximum,
                max_allowed: Pages::max_value(),
            });
        }

        let mut mmap = pool.acquire()?;
        let minimum_bytes = ty.minimum.bytes().0;
        if minimum_bytes > 0 &&
            let Err(e) = mmap.make_accessible(0, minimum_bytes)
        {
            // The mapping is in an unknown protection state; let it drop rather than pool it.
            return Err(MemoryError::Region(e));
        }
        let base = mmap.as_mut_ptr();

        let vm_memory_definition = match vm_memory_location {
            Some(mut location) => {
                // SAFETY: forwarded to caller — the location is the instance's definition slot.
                unsafe {
                    let definition = location.as_mut();
                    definition.base = base;
                    definition.current_length = minimum_bytes;
                }
                MaybeInstanceOwned::Instance(location)
            },
            None => MaybeInstanceOwned::Host(Box::new(UnsafeCell::new(VMMemoryDefinition {
                base,
                current_length: minimum_bytes,
            }))),
        };

        Ok(Self {
            mmap: Some(mmap),
            accessible_bytes: minimum_bytes,
            size: ty.minimum,
            memory_type: *ty,
            maximum,
            vm_memory_definition,
            pool,
        })
    }

    fn definition(&self) -> NonNull<VMMemoryDefinition> {
        self.vm_memory_definition.as_ptr()
    }
}

impl Drop for PooledLinearMemory {
    fn drop(&mut self) {
        if let Some(mmap) = self.mmap.take() {
            self.pool.release(mmap, self.accessible_bytes);
        }
    }
}

impl LinearMemory for PooledLinearMemory {
    fn ty(&self) -> MemoryType {
        let mut ty = self.memory_type;
        ty.minimum = self.size;
        ty
    }

    fn size(&self) -> Pages {
        self.size
    }

    fn style(&self) -> MemoryStyle {
        MemoryStyle::Static
    }

    fn grow(&mut self, delta: Pages) -> Result<Pages, MemoryError> {
        if delta.0 == 0 {
            return Ok(self.size);
        }
        let new_pages = self.size.checked_add(delta).ok_or(MemoryError::CouldNotGrow {
            current: self.size,
            attempted_delta: delta,
        })?;
        if new_pages > self.maximum || new_pages > Pages::max_value() {
            return Err(MemoryError::CouldNotGrow {
                current: self.size,
                attempted_delta: delta,
            });
        }

        let prev_pages = self.size;
        let new_bytes = new_pages.bytes().0;
        let mmap = self.mmap.as_mut().expect("mmap is only vacated on drop");
        mmap.make_accessible(self.accessible_bytes, new_bytes - self.accessible_bytes)
            .map_err(MemoryError::Region)?;
        self.accessible_bytes = new_bytes;
        self.size = new_pages;

        // SAFETY: the definition outlives this memory (instance-owned) or is owned by it (host).
        unsafe {
            self.definition().as_mut().current_length = new_bytes;
        }
        Ok(prev_pages)
    }

    fn grow_at_least(&mut self, min_size: u64) -> Result<(), MemoryError> {
        let current = self.size.bytes().0 as u64;
        if current < min_size {
            let delta_pages = (min_size - current).div_ceil(wasmer::WASM_PAGE_SIZE as u64);
            self.grow(Pages(u32::try_from(delta_pages).map_err(|_| {
                MemoryError::CouldNotGrow {
                    current: self.size,
                    attempted_delta: Pages(u32::MAX),
                }
            })?))?;
        }
        Ok(())
    }

    /// Returns the memory to the state a fresh instantiation creates it in, before its data
    /// segments are written: sized at the module's declared minimum, every page zero-filled, and
    /// every page past the minimum `PROT_NONE` again.
    ///
    /// This is the one place the memory departs from `VMOwnedMemory`, whose reset only drops the
    /// size to zero and leaves the protection and contents alone. The engine calls it to restore a
    /// reused instance, and nothing else reaches it.
    ///
    /// If the reset fails the memory is in an unknown state and the error is returned; the caller
    /// must then not run guest code on it.
    fn reset(&mut self) -> Result<(), MemoryError> {
        let minimum = self.memory_type.minimum;
        let minimum_bytes = minimum.bytes().0;
        let mmap = self.mmap.as_mut().expect("mmap is only vacated on drop");
        discard_contents(mmap, minimum_bytes, self.accessible_bytes)?;
        self.accessible_bytes = minimum_bytes;
        self.size = minimum;
        // SAFETY: as in `grow`.
        unsafe {
            self.definition().as_mut().current_length = minimum_bytes;
        }
        Ok(())
    }

    fn vmmemory(&self) -> NonNull<VMMemoryDefinition> {
        self.definition()
    }

    fn try_clone(&self) -> Result<Box<dyn LinearMemory + Send + Sync + 'static>, MemoryError> {
        Err(MemoryError::MemoryNotShared)
    }

    fn copy(&self) -> Result<Box<dyn LinearMemory + Send + Sync + 'static>, MemoryError> {
        // An unpooled deep copy: correctness is all that matters on this cold path.
        let copied = VMOwnedMemory::new(&self.ty(), &MemoryStyle::Static)?;
        // SAFETY: both definitions are valid; the source's accessible range covers current_length.
        unsafe {
            let src = self.definition().as_ref();
            let dst = copied.vmmemory().as_ref();
            std::ptr::copy_nonoverlapping(src.base, dst.base, src.current_length);
        }
        Ok(Box::new(copied))
    }

    fn as_shared(&self) -> Result<VMSharedMemory, MemoryError> {
        Err(MemoryError::MemoryNotShared)
    }
}

/// Tunables that back static-style guest memories with a [`MemoryPool`], delegating everything
/// else to the wrapped tunables.
pub struct PooledMemoryTunables<T: Tunables> {
    base: T,
    pool: Arc<MemoryPool>,
}

impl<T: Tunables> PooledMemoryTunables<T> {
    pub fn new(base: T, pool: Arc<MemoryPool>) -> Self {
        Self { base, pool }
    }
}

impl<T: Tunables> Tunables for PooledMemoryTunables<T> {
    fn memory_style(&self, memory: &MemoryType) -> MemoryStyle {
        self.base.memory_style(memory)
    }

    fn table_style(&self, table: &TableType) -> TableStyle {
        self.base.table_style(table)
    }

    fn create_host_memory(&self, ty: &MemoryType, style: &MemoryStyle) -> Result<VMMemory, MemoryError> {
        match style {
            MemoryStyle::Static => {
                // SAFETY: no definition location is passed.
                let memory = unsafe { PooledLinearMemory::new(self.pool.clone(), ty, None)? };
                Ok(VMMemory(Box::new(memory)))
            },
            MemoryStyle::Dynamic { .. } => self.base.create_host_memory(ty, style),
        }
    }

    unsafe fn create_vm_memory(
        &self,
        ty: &MemoryType,
        style: &MemoryStyle,
        vm_definition_location: NonNull<VMMemoryDefinition>,
    ) -> Result<VMMemory, MemoryError> {
        match style {
            MemoryStyle::Static => {
                // SAFETY: forwarded to caller — the location is the instance's definition slot.
                let memory = unsafe { PooledLinearMemory::new(self.pool.clone(), ty, Some(vm_definition_location))? };
                Ok(VMMemory(Box::new(memory)))
            },
            // SAFETY: forwarded to caller.
            MemoryStyle::Dynamic { .. } => unsafe { self.base.create_vm_memory(ty, style, vm_definition_location) },
        }
    }

    fn create_host_table(&self, ty: &TableType, style: &TableStyle) -> Result<VMTable, String> {
        self.base.create_host_table(ty, style)
    }

    unsafe fn create_vm_table(
        &self,
        ty: &TableType,
        style: &TableStyle,
        vm_definition_location: NonNull<VMTableDefinition>,
    ) -> Result<VMTable, String> {
        // SAFETY: forwarded to caller.
        unsafe { self.base.create_vm_table(ty, style, vm_definition_location) }
    }

    fn vmconfig(&self) -> &wasmer::sys::vm::VMConfig {
        self.base.vmconfig()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pool() -> Arc<MemoryPool> {
        Arc::new(MemoryPool::new(4))
    }

    fn memory_type(minimum: u32, maximum: u32) -> MemoryType {
        MemoryType::new(Pages(minimum), Some(Pages(maximum)), false)
    }

    fn slice_of(memory: &dyn LinearMemory) -> &[u8] {
        // SAFETY: the definition covers current_length accessible bytes.
        unsafe {
            let definition = memory.vmmemory().as_ref();
            std::slice::from_raw_parts(definition.base, definition.current_length)
        }
    }

    #[test]
    fn reused_mapping_is_zeroed_and_resized() {
        let pool = test_pool();
        // SAFETY: no definition location is passed.
        let mut memory = unsafe { PooledLinearMemory::new(pool.clone(), &memory_type(2, 32), None).unwrap() };
        unsafe {
            let definition = memory.vmmemory().as_ref();
            std::ptr::write_bytes(definition.base, 0xAB, definition.current_length);
        }
        memory.grow(Pages(3)).unwrap();
        assert_eq!(memory.size(), Pages(5));
        drop(memory);
        assert_eq!(pool.pooled_count(), 1);

        // SAFETY: as above.
        let memory = unsafe { PooledLinearMemory::new(pool.clone(), &memory_type(4, 32), None).unwrap() };
        assert_eq!(pool.pooled_count(), 0);
        assert_eq!(memory.size(), Pages(4));
        assert!(
            slice_of(&memory).iter().all(|&b| b == 0),
            "reused mapping must be zeroed"
        );
    }

    /// A previous tenant's bytes must not be reachable by growing into its high-water mark: pages
    /// the new tenant reaches via `memory.grow` are exactly the pages the scrub re-mapped, so a
    /// scrub that merely re-protected without discarding contents fails here.
    #[test]
    fn pages_grown_into_previous_tenants_range_are_zeroed() {
        let pool = test_pool();
        // SAFETY: no definition location is passed.
        let mut first = unsafe { PooledLinearMemory::new(pool.clone(), &memory_type(1, 32), None).unwrap() };
        first.grow(Pages(7)).unwrap();
        unsafe {
            let definition = first.vmmemory().as_ref();
            std::ptr::write_bytes(definition.base, 0xCD, definition.current_length);
        }
        drop(first);

        // SAFETY: as above.
        let mut second = unsafe { PooledLinearMemory::new(pool.clone(), &memory_type(1, 32), None).unwrap() };
        second.grow(Pages(7)).unwrap();
        assert!(
            slice_of(&second).iter().all(|&b| b == 0),
            "pages grown into a previous tenant's high-water mark must be zeroed"
        );
    }

    /// The trap boundary must be restored on reuse: cranelift elides bounds checks for static
    /// memories, so an out-of-bounds read below a previous tenant's high-water mark is stopped
    /// only by the pages being `PROT_NONE` again. The faulting read runs in a forked child
    /// (nextest runs one test per process, so the fork is isolated) and must die by SIGSEGV/SIGBUS
    /// rather than complete — completing would mean the previous tenant's range is still readable.
    #[test]
    fn out_of_bounds_read_into_previous_tenants_range_faults() {
        let pool = test_pool();
        // SAFETY: no definition location is passed.
        let mut first = unsafe { PooledLinearMemory::new(pool.clone(), &memory_type(1, 32), None).unwrap() };
        first.grow(Pages(7)).unwrap();
        unsafe {
            let definition = first.vmmemory().as_ref();
            std::ptr::write_bytes(definition.base, 0xCD, definition.current_length);
        }
        drop(first);

        // SAFETY: as above.
        let second = unsafe { PooledLinearMemory::new(pool.clone(), &memory_type(1, 32), None).unwrap() };
        // Two pages past the new tenant's single accessible page, well inside the previous
        // tenant's eight.
        let probe = unsafe { second.vmmemory().as_ref().base.add(2 * 64 * 1024) };

        assert_read_faults(
            probe,
            "reading past current_length into a previous tenant's range must fault",
        );
    }

    /// Asserts that reading `probe` faults. The read runs in a forked child (nextest runs one test
    /// per process, so the fork is isolated), which must die by SIGSEGV/SIGBUS rather than complete.
    fn assert_read_faults(probe: *const u8, why: &str) {
        // SAFETY: fork + waitpid; the child only performs the probe read and _exits.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                // The child holds duplicates of the test runner's output pipes; close them so
                // this fork can never register as a leaked handle, however it exits.
                libc::close(0);
                libc::close(1);
                libc::close(2);
                std::ptr::read_volatile(probe);
                // Reaching this line means the read did not trap.
                libc::_exit(0);
            }
            let mut status = 0;
            assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
            assert!(
                libc::WIFSIGNALED(status) &&
                    (libc::WTERMSIG(status) == libc::SIGSEGV || libc::WTERMSIG(status) == libc::SIGBUS),
                "{why}, got status {status}"
            );
        }
    }

    #[test]
    fn reset_restores_a_fresh_reservation() {
        let pool = test_pool();
        // SAFETY: no definition location is passed.
        let mut memory = unsafe { PooledLinearMemory::new(pool.clone(), &memory_type(1, 32), None).unwrap() };
        memory.grow(Pages(7)).unwrap();
        unsafe {
            let definition = memory.vmmemory().as_ref();
            std::ptr::write_bytes(definition.base, 0xCD, definition.current_length);
        }

        memory.reset().unwrap();
        assert_eq!(memory.size(), Pages(1), "a reset returns the memory to its minimum");
        assert!(
            slice_of(&memory).iter().all(|&b| b == 0),
            "pages accessible after a reset must be zeroed"
        );
        // Two pages past the minimum, inside the eight the memory held before.
        let probe = unsafe { memory.vmmemory().as_ref().base.add(2 * 64 * 1024) };
        assert_read_faults(probe, "a page accessible only before a reset must fault after it");
    }

    #[test]
    fn grow_respects_maximum() {
        let pool = test_pool();
        // SAFETY: no definition location is passed.
        let mut memory = unsafe { PooledLinearMemory::new(pool.clone(), &memory_type(1, 4), None).unwrap() };
        memory.grow(Pages(3)).unwrap();
        let err = memory.grow(Pages(1)).unwrap_err();
        assert!(matches!(err, MemoryError::CouldNotGrow { .. }));
    }

    #[test]
    fn pool_capacity_is_bounded() {
        let pool = Arc::new(MemoryPool::new(1));
        // SAFETY: no definition location is passed.
        let a = unsafe { PooledLinearMemory::new(pool.clone(), &memory_type(1, 4), None).unwrap() };
        // SAFETY: as above.
        let b = unsafe { PooledLinearMemory::new(pool.clone(), &memory_type(1, 4), None).unwrap() };
        drop(a);
        drop(b);
        assert_eq!(pool.pooled_count(), 1, "pool must not exceed max_slots");
    }
}
