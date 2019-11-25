use core::{ffi::c_void, intrinsics, ptr::Unique};

use libc_print::libc_eprintln;

use crate::heap::{self, BlockRegion, BLOCK_REGION_META_SIZE, SPLIT_MIN_BLOCK_SIZE};

#[repr(C)]
pub struct IntrusiveList {
    pub head: Option<Unique<BlockRegion>>,
    pub tail: Option<Unique<BlockRegion>>,
}

impl IntrusiveList {
    pub const fn new() -> Self {
        IntrusiveList {
            head: None,
            tail: None,
        }
    }

    /// Add a block to the list
    pub fn insert(&mut self, to_insert: Unique<BlockRegion>) -> Result<(), ()> {
        unsafe {
            debug_assert!(
                to_insert.as_ref().prev.is_none(),
                "block: {} at {:?}",
                to_insert.as_ref(),
                to_insert
            );
            debug_assert!(
                to_insert.as_ref().next.is_none(),
                "block: {} at {:?}",
                to_insert.as_ref(),
                to_insert
            );
        }

        // Add initial element
        if self.head.is_none() {
            debug_assert!(self.tail.is_none());
            self.head = Some(to_insert);
            self.tail = Some(to_insert);
            return Ok(());
        }

        debug_assert!(self.head.is_some());
        debug_assert!(self.tail.is_some());

        unsafe {
            match self.find_higher_block(to_insert)? {
                Some(block) => IntrusiveList::insert_before(block, to_insert),
                None => IntrusiveList::insert_after(self.tail.unwrap(), to_insert),
            }
            let inserted = IntrusiveList::maybe_merge_adjacent(to_insert);
            self.update_ends(inserted);
        }
        Ok(())
    }

    /// Removes and returns the first suitable block
    #[inline]
    pub fn pop(&mut self, size: usize) -> Option<Unique<BlockRegion>> {
        let mut ptr = self.head;
        while let Some(block) = ptr {
            unsafe {
                if size == block.as_ref().size {
                    dprintln!(
                        "[libdmalloc.so]: found perfect {} at {:?} for size {}",
                        block.as_ref(),
                        block,
                        size
                    );
                    return Some(self.remove(block));
                }
                if size + SPLIT_MIN_BLOCK_SIZE <= block.as_ref().size {
                    dprintln!(
                        "[libdmalloc.so]: found suitable {} at {:?} for size {}",
                        block.as_ref(),
                        block,
                        size
                    );
                    return Some(self.remove(block));
                }

                ptr = block.as_ref().next;
            }
        }
        None
    }

    /// Prints some debugging information about the heap structure
    #[cfg(feature = "debug")]
    pub fn debug(&self) {
        let mut i = 0;
        let mut ptr = self.head;
        while let Some(block) = ptr {
            unsafe {
                dprintln!("[debug]: pos: {}\t{} at\t{:?}", i, block.as_ref(), block);
                block.as_ref().verify(true, true);

                match block.as_ref().prev {
                    Some(prev) => {
                        debug_assert_eq!(prev.as_ref().next.unwrap().as_ptr(), block.as_ptr());
                        // rule out self reference
                        debug_assert_ne!(prev.as_ptr(), block.as_ptr());
                    }
                    None => debug_assert_eq!(self.head.unwrap().as_ptr(), block.as_ptr()),
                }

                match block.as_ref().next {
                    Some(next) => {
                        debug_assert_eq!(next.as_ref().prev.unwrap().as_ptr(), block.as_ptr());
                        // rule out self reference
                        debug_assert_ne!(next.as_ptr(), block.as_ptr());
                    }
                    None => debug_assert_eq!(self.tail.unwrap().as_ptr(), block.as_ptr()),
                }

                if let Some(next) = block.as_ref().next {
                    debug_assert!(
                        block.as_ptr() < next.as_ptr(),
                        "{:?} is not smaller than {:?}",
                        block,
                        next
                    );
                }
                ptr = block.as_ref().next;
                i += 1;
            }
        }
    }

    /// Add block to the list before the given element
    unsafe fn insert_before(mut before: Unique<BlockRegion>, mut to_insert: Unique<BlockRegion>) {
        // Update links in new block
        to_insert.as_mut().prev = before.as_ref().prev;
        to_insert.as_mut().next = Some(before);

        // Update link for element after new block
        before.as_mut().prev = Some(to_insert);

        // Update link for element before new block
        if let Some(mut prev) = to_insert.as_ref().prev {
            prev.as_mut().next = Some(to_insert);
        }
    }

    /// Add block to the list after the given element
    unsafe fn insert_after(mut after: Unique<BlockRegion>, mut to_insert: Unique<BlockRegion>) {
        // Update links in new block
        to_insert.as_mut().next = after.as_ref().next;
        to_insert.as_mut().prev = Some(after);

        // Update link for element before new block
        after.as_mut().next = Some(to_insert);

        // Update link for element after new block
        if let Some(mut next) = to_insert.as_ref().next {
            next.as_mut().prev = Some(to_insert);
        }
    }

    /// Checks if head or tail should be updated with current block
    #[inline]
    unsafe fn update_ends(&mut self, block: Unique<BlockRegion>) {
        // Update head if necessary
        if block.as_ref().prev.is_none() {
            self.head = Some(block);
        }

        // Update tail if necessary
        if block.as_ref().next.is_none() {
            self.tail = Some(block);
        }
    }

    /// Takes a pointer to a block and tries to merge it with next.
    /// Returns a merged pointer if merge was possible, None otherwise.
    /// NOTE: This function does not modify head or tail.
    unsafe fn maybe_merge_next(mut block: Unique<BlockRegion>) -> Option<Unique<BlockRegion>> {
        let next = block.as_ref().next?;

        if heap::get_next_potential_block_ptr(block).as_ptr() != next.cast::<c_void>().as_ptr() {
            return None;
        }

        dprintln!("[merge]: {} at {:?}", block.as_ref(), block);
        dprintln!("       & {} at {:?}", next.as_ref(), next);
        // Update related links
        block.as_mut().next = next.as_ref().next;
        if let Some(mut n) = block.as_ref().next {
            n.as_mut().prev = Some(block);
        }
        // Update to final size
        block.as_mut().size += BLOCK_REGION_META_SIZE + next.as_ref().size;

        // Overwrite BlockRegion meta data for old block to detect double free
        intrinsics::volatile_set_memory(next.cast::<c_void>().as_ptr(), 0, BLOCK_REGION_META_SIZE);

        dprintln!("      -> {} at {:?}", block.as_ref(), block);
        return Some(block);
    }

    /// Takes a pointer to a block and tries to merge it with prev.
    /// Returns a merged pointer if merge was possible, None otherwise.
    /// NOTE: This function does not modify head or tail.
    #[inline]
    unsafe fn maybe_merge_prev(block: Unique<BlockRegion>) -> Option<Unique<BlockRegion>> {
        IntrusiveList::maybe_merge_next(block.as_ref().prev?)
    }

    /// Merges adjacent blocks if possible.
    /// Always returns a pointer to a block.
    #[inline]
    unsafe fn maybe_merge_adjacent(block: Unique<BlockRegion>) -> Unique<BlockRegion> {
        let block = IntrusiveList::maybe_merge_prev(block).unwrap_or(block);
        return IntrusiveList::maybe_merge_next(block).unwrap_or(block);
    }

    /// Returns first block that has a higher memory address than the given block.
    /// TODO: implement as binary search
    #[inline]
    fn find_higher_block(
        &self,
        to_insert: Unique<BlockRegion>,
    ) -> Result<Option<Unique<BlockRegion>>, ()> {
        let mut ptr = self.head;
        while let Some(block) = ptr {
            if block.as_ptr() == to_insert.as_ptr() {
                // block is already in list.
                // One reason for this is double free()
                return Err(());
            }
            if block.as_ptr() > to_insert.as_ptr() {
                return Ok(Some(block));
            }
            ptr = unsafe { block.as_ref().next };
        }
        return Ok(None);
    }

    /// Removes the given element from the list and returns it.
    unsafe fn remove(&mut self, mut elem: Unique<BlockRegion>) -> Unique<BlockRegion> {
        // Update head
        if let Some(head) = self.head {
            if elem.as_ptr() == head.as_ptr() {
                self.head = elem.as_ref().next;
            }
        }
        // Update tail
        if let Some(tail) = self.tail {
            if elem.as_ptr() == tail.as_ptr() {
                self.tail = elem.as_ref().prev;
            }
        }

        // Update link in previous element
        if let Some(mut prev) = elem.as_ref().prev {
            prev.as_mut().next = elem.as_ref().next;
        }
        // Update link in next element
        if let Some(mut next) = elem.as_ref().next {
            next.as_mut().prev = elem.as_ref().prev;
        }

        // Clear links in current element
        elem.as_mut().next = None;
        elem.as_mut().prev = None;
        return elem;
    }
}
