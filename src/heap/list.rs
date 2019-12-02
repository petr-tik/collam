use core::{ffi::c_void, intrinsics};

use libc_print::libc_eprintln;

use crate::heap::block::{BlockPtr, BLOCK_META_SIZE, BLOCK_MIN_SIZE};

#[repr(C)]
pub struct IntrusiveList {
    pub head: Option<BlockPtr>,
    pub tail: Option<BlockPtr>,
}

impl IntrusiveList {
    pub const fn new() -> Self {
        IntrusiveList {
            head: None,
            tail: None,
        }
    }

    /// Inserts a `BlockPtr` to the existing list and
    /// returns `Err` on detected double-free.
    pub fn insert(&mut self, to_insert: BlockPtr) -> Result<(), ()> {
        debug_assert!(
            to_insert.as_ref().prev.is_none(),
            "block: {} at {:p}",
            to_insert.as_ref(),
            to_insert
        );
        debug_assert!(
            to_insert.as_ref().next.is_none(),
            "block: {} at {:p}",
            to_insert.as_ref(),
            to_insert
        );

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

    /// Removes and returns the first suitable `BlockPtr`.
    #[inline]
    pub fn pop(&mut self, size: usize) -> Option<BlockPtr> {
        let mut ptr = self.head;
        while let Some(block) = ptr {
            unsafe {
                if size == block.size() {
                    dprintln!(
                        "[libcollam.so]: found perfect {} at {:p} for size {}",
                        block.as_ref(),
                        block,
                        size
                    );
                    return Some(self.remove(block));
                }
                if size + BLOCK_MIN_SIZE <= block.size() {
                    dprintln!(
                        "[libcollam.so]: found suitable {} at {:p} for size {}",
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

    /// Prints some debugging information about the heap structure.
    #[cfg(feature = "debug")]
    pub fn debug(&self) {
        let mut i = 0;
        let mut ptr = self.head;
        while let Some(block) = ptr {
            dprintln!("[debug]: pos: {}\t{} at\t{:p}", i, block.as_ref(), block);
            block.verify(true);

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
                    "{:p} is not smaller than {:p}",
                    block,
                    next
                );
            }
            ptr = block.as_ref().next;
            i += 1;
        }
    }

    /// Adds a `BlockPtr` to the list before the given anchor.
    unsafe fn insert_before(mut anchor: BlockPtr, mut to_insert: BlockPtr) {
        // Update links in new block
        to_insert.as_mut().prev = anchor.as_ref().prev;
        to_insert.as_mut().next = Some(anchor);

        // Update link for element after new block
        anchor.as_mut().prev = Some(to_insert);

        // Update link for element before new block
        if let Some(mut prev) = to_insert.as_ref().prev {
            prev.as_mut().next = Some(to_insert);
        }
    }

    /// Adds a `BlockPtr` to the list after the given anchor.
    unsafe fn insert_after(mut anchor: BlockPtr, mut to_insert: BlockPtr) {
        // Update links in new block
        to_insert.as_mut().next = anchor.as_ref().next;
        to_insert.as_mut().prev = Some(anchor);

        // Update link for element before new block
        anchor.as_mut().next = Some(to_insert);

        // Update link for element after new block
        if let Some(mut next) = to_insert.as_ref().next {
            next.as_mut().prev = Some(to_insert);
        }
    }

    /// Checks if head or tail should be updated with the given `BlockPtr`.
    #[inline]
    unsafe fn update_ends(&mut self, block: BlockPtr) {
        // Update head if necessary
        if block.as_ref().prev.is_none() {
            self.head = Some(block);
        }

        // Update tail if necessary
        if block.as_ref().next.is_none() {
            self.tail = Some(block);
        }
    }

    /// Takes a `BlockPtr` and tries to merge it with the next block, if unused.
    /// Returns a merged `BlockPtr` if merge was possible, `None` otherwise.
    /// NOTE: This function does not modify head or tail.
    /// TODO: move method to Block?
    unsafe fn maybe_merge_next(mut block: BlockPtr) -> Option<BlockPtr> {
        let next = block.as_ref().next?;

        if block.next_potential_block().as_ptr() != next.cast::<c_void>().as_ptr() {
            return None;
        }

        dprintln!("[merge]: {} at {:p}", block.as_ref(), block);
        dprintln!("       & {} at {:p}", next.as_ref(), next);
        // Update related links
        block.as_mut().next = next.as_ref().next;
        if let Some(mut n) = block.as_ref().next {
            n.as_mut().prev = Some(block);
        }
        // Update to final size
        block.as_mut().size += BLOCK_META_SIZE + next.size();

        // Overwrite block meta data for old block to detect double free
        intrinsics::volatile_set_memory(next.cast::<c_void>().as_ptr(), 0, BLOCK_META_SIZE);

        dprintln!("      -> {} at {:p}", block.as_ref(), block);
        Some(block)
    }

    /// Takes a `BlockPtr` and tries to merge it with the prev block.
    /// Returns a merged `BlockPtr` if merge was possible, `None` otherwise.
    /// NOTE: This function does not modify head or tail.
    #[inline]
    unsafe fn maybe_merge_prev(block: BlockPtr) -> Option<BlockPtr> {
        IntrusiveList::maybe_merge_next(block.as_ref().prev?)
    }

    /// Takes a `BlockPtr` and tries to merge adjacent blocks if possible.
    /// Always returns a `BlockPtr`.
    #[inline]
    unsafe fn maybe_merge_adjacent(block: BlockPtr) -> BlockPtr {
        let block = IntrusiveList::maybe_merge_prev(block).unwrap_or(block);
        IntrusiveList::maybe_merge_next(block).unwrap_or(block)
    }

    /// Returns first `BlockPtr` that has a higher memory address than the given `BlockPtr`
    /// or `None` if no block exists at a higher memory address.
    /// Returns `Err` if given `BlockPtr` is already in list.
    /// TODO: implement with better algorithm
    #[inline]
    fn find_higher_block(&self, to_insert: BlockPtr) -> Result<Option<BlockPtr>, ()> {
        let mut ptr = self.head;
        while let Some(block) = ptr {
            if block == to_insert {
                // block is already in list.
                // One reason for this is double free()
                return Err(());
            }
            if block.as_ptr() > to_insert.as_ptr() {
                return Ok(Some(block));
            }
            ptr = block.as_ref().next;
        }
        Ok(None)
    }

    /// Removes the given `BlockPtr` from list and returns it.
    unsafe fn remove(&mut self, mut elem: BlockPtr) -> BlockPtr {
        // Update head
        if let Some(head) = self.head {
            if elem == head {
                self.head = elem.as_ref().next;
            }
        }
        // Update tail
        if let Some(tail) = self.tail {
            if elem == tail {
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
        elem
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::{alloc, request_block};

    #[test]
    fn test_insert_after_no_merge() {
        let mut list = IntrusiveList::new();
        assert_eq!(list.head, None);
        assert_eq!(list.tail, None);

        let mut block = request_block(256).expect("unable to request block");
        // Block2 imitates a used block. So it will not be added to list
        let mut block2 = block.shrink(64).expect("unable to split block");
        let block3 = block2.shrink(64).expect("unable to split block");

        // Insert block1
        list.insert(block).expect("unable to insert");
        assert_eq!(list.head, Some(block));
        assert_eq!(list.tail, Some(block));
        assert_eq!(block.as_ref().next, None);
        assert_eq!(block.as_ref().prev, None);

        // Insert block3
        list.insert(block3).expect("unable to insert");
        assert_eq!(list.head, Some(block));
        assert_eq!(list.tail, Some(block3));
        assert_eq!(block.as_ref().next, Some(block3));
        assert_eq!(block.as_ref().prev, None);
        assert_eq!(block3.as_ref().next, None);
        assert_eq!(block3.as_ref().prev, Some(block));
    }

    #[test]
    fn test_insert_before_no_merge() {
        let mut list = IntrusiveList::new();
        assert_eq!(list.head, None);
        assert_eq!(list.tail, None);

        let mut block = request_block(256).expect("unable to request block");
        // Block2 imitates a used block. So it will not be added to list
        let mut block2 = block.shrink(64).expect("unable to split block");
        let block3 = block2.shrink(64).expect("unable to split block");

        // Insert block3
        list.insert(block3).expect("unable to insert");
        assert_eq!(list.head, Some(block3));
        assert_eq!(list.tail, Some(block3));
        assert_eq!(block3.as_ref().next, None);
        assert_eq!(block3.as_ref().prev, None);

        // Insert block1
        list.insert(block).expect("unable to insert");
        assert_eq!(list.head, Some(block));
        assert_eq!(list.tail, Some(block3));
        assert_eq!(block.as_ref().next, Some(block3));
        assert_eq!(block.as_ref().prev, None);
        assert_eq!(block3.as_ref().next, None);
        assert_eq!(block3.as_ref().prev, Some(block));
    }

    #[test]
    fn test_insert_merge() {
        let mut list = IntrusiveList::new();
        assert_eq!(list.head, None);
        assert_eq!(list.tail, None);

        let mut block = request_block(256).expect("unable to request block");
        let mut block2 = block.shrink(64).expect("unable to split block");
        let block3 = block2.shrink(64).expect("unable to split block");

        // Insert block1
        list.insert(block).expect("unable to insert");
        assert_eq!(list.head, Some(block));
        assert_eq!(list.tail, Some(block));
        assert_eq!(block.as_ref().next, None);
        assert_eq!(block.as_ref().prev, None);
        assert_eq!(block.size(), 64);

        // Insert block2
        list.insert(block2).expect("unable to insert");
        assert_eq!(list.head, Some(block));
        assert_eq!(list.tail, Some(block));
        assert_eq!(block.as_ref().next, None);
        assert_eq!(block.as_ref().prev, None);
        assert_eq!(block.size(), 64 + BLOCK_META_SIZE + 64);

        // Insert block3
        list.insert(block3).expect("unable to insert");
        assert_eq!(list.head, Some(block));
        assert_eq!(list.tail, Some(block));
        assert_eq!(block.as_ref().next, None);
        assert_eq!(block.as_ref().prev, None);
        assert!(block.size() > 64 + BLOCK_META_SIZE + 64 + BLOCK_META_SIZE);
    }

    #[test]
    fn test_pop_exact_size() {
        let mut list = IntrusiveList::new();
        let mut block = request_block(512).expect("unable to request block");
        // Block2 imitates a used block. So it will not be added to list
        let mut block2 = block.shrink(64).expect("unable to split block");
        let block3 = block2.shrink(64).expect("unable to split block");

        // Insert block1
        list.insert(block).expect("unable to insert");
        // Insert block3
        list.insert(block3).expect("unable to insert");

        let result = list.pop(64).expect("got no block");
        assert_eq!(result, block);
        assert_eq!(result.as_ref().next, None);
        assert_eq!(result.as_ref().prev, None);
        assert_eq!(result.size(), 64);
    }

    #[test]
    fn test_pop_smaller_size() {
        let mut list = IntrusiveList::new();
        let mut block = request_block(512).expect("unable to request block");
        // Block2 imitates a used block. So it will not be added to list
        let mut block2 = block.shrink(64).expect("unable to split block");
        let block3 = block2.shrink(64).expect("unable to split block");

        // Insert block1
        list.insert(block).expect("unable to insert");
        // Insert block3
        list.insert(block3).expect("unable to insert");

        let result = list.pop(16).expect("got no block");
        assert_eq!(result, block);
        assert_eq!(result.as_ref().next, None);
        assert_eq!(result.as_ref().prev, None);
        assert_eq!(result.size(), 64);
    }

    #[cfg(feature = "debug")]
    #[test]
    fn test_debug() {
        let mut list = IntrusiveList::new();
        let mut block = request_block(256).expect("unable to request block");
        // Block2 imitates a used block. So it will not be added to list
        let mut block2 = block.shrink(64).expect("unable to split block");
        let block3 = block2.shrink(64).expect("unable to split block");

        // Insert block1
        list.insert(block).expect("unable to insert");
        // Insert block3
        list.insert(block3).expect("unable to insert");
        list.debug()
    }
}
