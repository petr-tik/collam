use core::{ffi::c_void, fmt, intrinsics, mem, ptr::Unique};

use libc_print::libc_eprintln;

use crate::util;

/// The required block size to store the bare minimum of metadata (size + magic values).
pub const BLOCK_META_SIZE: usize = util::align_scalar_unchecked(mem::align_of::<usize>() * 2);
/// The minimum region size to save intrusive data structures if not allocated by the user.
const BLOCK_MIN_REGION_SIZE: usize =
    util::align_scalar_unchecked(mem::align_of::<Option<BlockPtr>>() * 2);
/// Defines the minimum remaining size of a block to consider splitting it.
pub const BLOCK_SPLIT_MIN_SIZE: usize = util::align_scalar_unchecked(
    BLOCK_META_SIZE + BLOCK_MIN_REGION_SIZE + mem::align_of::<libc::max_align_t>(),
);

const BLOCK_MAGIC_FREE: u16 = 0xDEAD;

/// Represents a mutable non-null Pointer to a `Block`.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct BlockPtr(Unique<Block>);

impl BlockPtr {
    /// Creates a `Block` instance at the given raw pointer for the specified size.
    pub fn new(ptr: Unique<c_void>, size: usize) -> Self {
        debug_assert_eq!(size, util::pad_to_scalar(size).unwrap().size());
        let ptr = ptr.cast::<Block>();
        unsafe { *ptr.as_ptr() = Block::new(size) };
        BlockPtr(ptr)
    }

    /// Returns an existing `BlockPtr` instance from the given memory region raw pointer
    pub fn from_mem_region(ptr: Unique<c_void>) -> Option<Self> {
        let block_ptr = unsafe { ptr.as_ptr().sub(BLOCK_META_SIZE).cast::<Block>() };
        Some(BlockPtr(Unique::new(block_ptr)?))
    }

    /// Returns a pointer to the assigned memory region for the given block
    pub fn mem_region(self) -> Unique<c_void> {
        debug_assert!(self.as_ref().verify());
        unsafe { Unique::new_unchecked(self.as_ptr().cast::<c_void>().add(BLOCK_META_SIZE)) }
    }

    /// Acquires underlying `*mut Block`.
    #[inline(always)]
    pub const fn as_ptr(self) -> *mut Block {
        self.0.as_ptr()
    }

    /// Casts to a pointer of another type.
    #[inline]
    pub const fn cast<U>(self) -> Unique<U> {
        unsafe { Unique::new_unchecked(self.as_ptr() as *mut U) }
    }

    /// Returns a pointer where the next `Block` would start.
    #[inline]
    pub fn next_potential_block(self) -> Unique<c_void> {
        unsafe { Unique::new_unchecked(self.cast::<c_void>().as_ptr().add(self.block_size())) }
    }

    /// Returns the allocatable size available for the user
    #[inline]
    pub fn size(&self) -> usize {
        self.as_ref().size
    }

    /// Returns the raw size in memory for this block.
    #[inline]
    pub fn block_size(&self) -> usize {
        BLOCK_META_SIZE + self.size()
    }

    /// Tries to merge self with the next block, if available.
    /// Returns a merged `BlockPtr` if merge was possible, `None` otherwise.
    pub fn maybe_merge_next(mut self) -> Option<BlockPtr> {
        let next = self.as_ref().next?;

        if self.next_potential_block().as_ptr() != next.cast::<c_void>().as_ptr() {
            return None;
        }

        dprintln!("[merge]: {} at {:p}", self.as_ref(), self.0);
        dprintln!("       & {} at {:p}", next.as_ref(), next);
        // Update related links
        self.as_mut().next = next.as_ref().next;
        if let Some(mut n) = self.as_ref().next {
            n.as_mut().prev = Some(self);
        }
        // Update to final size
        self.as_mut().size += BLOCK_META_SIZE + next.size();

        // Overwrite block meta data for old block to detect double free
        unsafe {
            intrinsics::volatile_set_memory(next.cast::<c_void>().as_ptr(), 0, BLOCK_META_SIZE)
        };

        dprintln!("      -> {} at {:p}", self.as_ref(), self.0);
        Some(self)
    }

    /// Shrinks the block in-place to have the exact memory size as specified (excluding metadata).
    /// Returns a newly created `BlockPtr` with the remaining size or `None` if split is not possible.
    pub fn shrink(&mut self, size: usize) -> Option<BlockPtr> {
        dprintln!("[split]: {} at {:p}", self.as_ref(), self.0);
        debug_assert_eq!(
            size,
            util::pad_to_scalar(size).expect("unable to align").size()
        );
        // Check if its possible to split the block with the requested size
        let rem_block_size = self.size().checked_sub(size + BLOCK_META_SIZE)?;

        if rem_block_size < BLOCK_SPLIT_MIN_SIZE {
            dprintln!("      -> None");
            return None;
        }

        // Update size for old block
        self.as_mut().size = size;

        // Create block with remaining size
        let new_block_ptr = unsafe { Unique::new_unchecked(self.mem_region().as_ptr().add(size)) };
        let new_block = BlockPtr::new(new_block_ptr, rem_block_size);

        dprintln!("      -> {} at {:p}", self.as_ref(), self.0);
        dprintln!("      -> {} at {:p}", new_block.as_ref(), new_block);
        dprintln!(
            "         distance is {} bytes",
            new_block.as_ptr() as usize - (self.as_ptr() as usize + self.block_size())
        );
        debug_assert_eq!(
            new_block.as_ptr() as usize - (self.as_ptr() as usize + self.block_size()),
            0
        );
        Some(new_block)
    }
}

impl AsMut<Block> for BlockPtr {
    #[inline(always)]
    fn as_mut(&mut self) -> &mut Block {
        unsafe { self.0.as_mut() }
    }
}

impl AsRef<Block> for BlockPtr {
    #[inline(always)]
    fn as_ref(&self) -> &Block {
        unsafe { self.0.as_ref() }
    }
}

impl PartialEq for BlockPtr {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.as_ptr() == other.as_ptr()
    }
}

impl fmt::Pointer for BlockPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:p}", self.as_ref())
    }
}

impl fmt::Debug for BlockPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {:p}", self.as_ref(), self.0)
    }
}

#[repr(C)]
pub struct Block {
    // Required metadata
    size: usize,
    magic: u16,
    // Memory region starts here. All following members will be
    // overwritten and are unusable if block has been allocated by a user.
    pub next: Option<BlockPtr>,
    pub prev: Option<BlockPtr>,
}

impl Block {
    pub const fn new(size: usize) -> Self {
        Block {
            size,
            next: None,
            prev: None,
            magic: BLOCK_MAGIC_FREE,
        }
    }

    #[inline(always)]
    pub fn unlink(&mut self) {
        self.next = None;
        self.prev = None;
    }

    /// Verifies block to detect memory corruption.
    /// Returns `true` if block metadata is intact, `false` otherwise.
    #[inline(always)]
    pub fn verify(&self) -> bool {
        self.magic == BLOCK_MAGIC_FREE
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        /*
        TODO: fix formatter for self.prev and self.next
        write!(
            f,
            "Block(size={}, prev={:?}, next={:?}, magic=0x{:X}, meta_size={})",
            self.size, self.prev, self.next, self.magic, BLOCK_META_SIZE,
        )*/
        write!(
            f,
            "Block(size={}, magic=0x{:X}, meta_size={})",
            self.size, self.magic, BLOCK_META_SIZE,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_block(block: BlockPtr, size: usize) {
        assert_eq!(block.size(), size, "block size doesn't match");
        assert_eq!(
            block.block_size(),
            BLOCK_META_SIZE + size,
            "block raw size doesn't match"
        );
        assert!(block.as_ref().verify(), "unable to verify block metadata");
        assert!(block.as_ref().next.is_none(), "next is not None");
        assert!(block.as_ref().prev.is_none(), "prev is not None");
    }

    #[test]
    fn test_block_new() {
        let alloc_size = 64;
        let ptr = unsafe {
            Unique::new(libc::malloc(BLOCK_META_SIZE + alloc_size))
                .expect("unable to allocate memory")
        };
        assert_block(BlockPtr::new(ptr, alloc_size), alloc_size);
        unsafe { libc::free(ptr.as_ptr()) };
    }

    #[test]
    fn test_block_shrink_with_remaining() {
        let block1_size = 4096;
        let ptr = unsafe {
            Unique::new(libc::malloc(BLOCK_META_SIZE + block1_size))
                .expect("unable to allocate memory")
        };
        let mut block1 = BlockPtr::new(ptr, block1_size);
        assert_block(block1, block1_size);
        let total_size = block1.block_size();
        assert_eq!(ptr.as_ptr(), block1.as_ptr().cast::<c_void>());

        // Shrink block1 to 256 bytes
        let mut block2 = block1.shrink(256).expect("split block failed");
        assert_block(block1, 256);
        assert_eq!(
            block1.next_potential_block().as_ptr(),
            block2.cast::<c_void>().as_ptr()
        );
        assert_block(block2, total_size - block1.block_size() - BLOCK_META_SIZE);

        // Shrink block2 to 256 bytes
        let block3 = block2.shrink(256).expect("split block failed");
        assert_block(block2, 256);
        assert_eq!(
            block2.next_potential_block().as_ptr(),
            block3.cast::<c_void>().as_ptr()
        );
        assert_block(
            block3,
            total_size - block1.block_size() - block2.block_size() - BLOCK_META_SIZE,
        );
        unsafe { libc::free(ptr.as_ptr()) };
    }

    #[test]
    fn test_block_shrink_no_remaining() {
        let alloc_size = 256;
        let ptr = unsafe {
            Unique::new(libc::malloc(BLOCK_META_SIZE + alloc_size))
                .expect("unable to allocate memory")
        };
        let mut block = BlockPtr::new(ptr, alloc_size);
        let remaining = block.shrink(240);

        // Assert correctness of initial block
        assert_eq!(ptr.as_ptr(), block.as_ptr().cast::<c_void>());
        assert_block(block, 256);

        // There should be no remaining block
        // since 240 will be aligned to 256 and no space is left.
        assert!(remaining.is_none());
        unsafe { libc::free(ptr.as_ptr()) };
    }

    #[test]
    fn test_block_verify_ok() {
        let alloc_size = 256;
        let ptr = unsafe {
            Unique::new(libc::malloc(BLOCK_META_SIZE + alloc_size))
                .expect("unable to allocate memory")
        };
        let block = BlockPtr::new(ptr, alloc_size);
        assert!(block.as_ref().verify());
        unsafe { libc::free(ptr.as_ptr()) };
    }

    #[test]
    fn test_block_verify_invalid() {
        let alloc_size = 256;
        let ptr = unsafe {
            Unique::new(libc::malloc(BLOCK_META_SIZE + alloc_size))
                .expect("unable to allocate memory")
        };
        let mut block = BlockPtr::new(ptr, alloc_size);
        block.as_mut().magic = 0x1234;
        assert_eq!(block.as_ref().verify(), false);

        unsafe { libc::free(ptr.as_ptr()) };
    }

    #[test]
    fn test_block_mem_region_ok() {
        let alloc_size = 64;
        let ptr = unsafe {
            Unique::new(libc::malloc(BLOCK_META_SIZE + alloc_size))
                .expect("unable to allocate memory")
        };
        let block = BlockPtr::new(ptr, alloc_size);
        let mem = block.mem_region();
        assert!(mem.as_ptr() > block.as_ptr().cast::<c_void>());
        let block2 = BlockPtr::from_mem_region(mem).expect("unable to create from mem region");
        assert_eq!(block, block2);
        unsafe { libc::free(ptr.as_ptr()) };
    }

    #[test]
    fn test_block_mem_region_err() {
        let region =
            unsafe { Unique::new_unchecked(mem::align_of::<libc::max_align_t>() as *mut c_void) };
        assert_eq!(BlockPtr::from_mem_region(region), None);
    }
}
