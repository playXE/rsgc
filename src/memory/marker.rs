use std::{ptr::null_mut, sync::atomic::AtomicUsize};

use crate::base::segmented_vec::SegmentedVec;

use super::{
    object_header::ObjectHeader,
    page::Pages,
    pointer_block::{MarkerWorkList, MarkingStack},
    traits::Trace,
    visitor::Visitor,
};

#[allow(dead_code)]
struct LocalMarker {
    marking_worklist: MarkerWorkList,
    marked_bytes: usize,
    mark_time: u64,
    num_busy: *const AtomicUsize,
    pages: *mut Pages,
    tmpstack: Vec<*mut ObjectHeader>,
}

impl LocalMarker {
    #[inline(never)]
    unsafe fn drain_marking_stack(&mut self) {
        while self.process_marking_stack(isize::MAX) {}
    }
    #[inline(never)]
    unsafe fn process_marking_stack(&mut self, mut remaining_budget: isize) -> bool {
        let mut raw_obj = self.marking_worklist.pop();
        while !raw_obj.is_null() {
            let size = (*raw_obj).visit_pointers(self);
            self.marked_bytes += size;
            remaining_budget -= size as isize;
            if remaining_budget < 0 {
                return true;
            }
            raw_obj = self.marking_worklist.pop();
        }

        false
    }

    unsafe fn mark_object(&mut self, raw_obj: *mut ObjectHeader) {
        if (*raw_obj).is_visited_unsynchronized() {
            return;
        }
     
        (*raw_obj).set_visited();
        self.marking_worklist.push(raw_obj);
        
    }

    unsafe fn process_weak_refs(&mut self) {
        let pages = self.pages as *mut Pages;

        let weak_refs = &mut (*pages).weak_refs;

        weak_refs.retain(|raw_obj| {
            let raw_obj = *raw_obj;
            if (*raw_obj).is_visited() {
                let data = (*raw_obj).data_mut();
                for offset in (*raw_obj).vtable().weak_refs.iter().copied() {
                    let weak_slot = data.add(offset).cast::<*mut ObjectHeader>();
                    let weak_ref = *weak_slot;
                    if !(*weak_ref).is_visited() {
                    
                        weak_slot.write(null_mut());
                    }
                }
                true
            } else {
                false
            }
        });

        let weak_maps = &mut (*pages).weak_maps;
        let mut callback = |obj: *mut ObjectHeader| {
            if (*obj).is_visited() {
                return obj;
            } else {
                null_mut()
            }
        };
        weak_maps.retain(|raw_obj| {
            let raw_obj = *raw_obj;
            if (*raw_obj).is_visited() {
                // invoke weak map processor that will remove dead keys from hash map
                ((*raw_obj).vtable().weak_map_process.unwrap())(raw_obj.add(1).cast(), &mut callback);
                true
            } else {
                false
            }
        });
    }

    unsafe fn bump_finalization_state_from_0_to_1(&self, obj: *mut ObjectHeader) {
        (*obj).set_finalization_ordering(true);
    }

    unsafe fn finalization_state(&self, obj: *mut ObjectHeader) -> u8 {
        if (*obj).is_visited() {
            if (*obj).finalization_ordering() {
                2
            } else {
                3
            }
        } else {
            if (*obj).finalization_ordering() {
                1
            } else {
                0
            }
        }
    }

    unsafe fn recursively_bump_finalization_state_from_2_to_3(&mut self, obj: *mut ObjectHeader) {
        let mut vis = FinalizationBumpVisitor { local: self };
        vis.local.tmpstack.push(obj);

        while let Some(obj) = vis.local.tmpstack.pop() {
            if (*obj).finalization_ordering() {
                (*obj).set_finalization_ordering(false);
                (*obj).visit(&mut vis);
            }
        }
    }

    unsafe fn recursively_bump_finalization_state_from_1_to_2(&mut self, obj: *mut ObjectHeader) {
        self.mark_object(obj);
        self.drain_marking_stack();
    }

    unsafe fn deal_with_objects_with_finalizers(&mut self) {
        let mut new_with_finalizers = SegmentedVec::new();

        let mut marked = SegmentedVec::new();
        let mut pending = SegmentedVec::new();

        struct PendingVisitor<'a> {
            local: &'a mut LocalMarker,
            stack: &'a mut SegmentedVec<*mut ObjectHeader>,
        }

        impl Visitor for PendingVisitor<'_> {
            unsafe fn visit_pointer(&mut self, object: *mut ObjectHeader) {
                self.stack.push(object);
            }

            unsafe fn visit_conservative(&mut self, from: *mut u8, to: *mut u8) {
                let mut current = from as *mut *mut u8;
                let to = to as *mut *mut u8;
                while current < to {
                    let addr = current.read();
                    let header = (*self.local.pages).try_pointer_conservative(addr as _);
                    if !header.is_null() {
                        self.visit_pointer(header);
                    }
                    current = current.add(1);
                }
            }
        }

        while let Some(obj) = (*self.pages).finalizable.pop() {
            if (*obj).is_visited() {
               /*if unlikely(!(*obj).is_initialized()) {
                    println!("keep alive {:p}", obj);
                    self.mark_object(obj);
                    self.drain_marking_stack();
                }*/
                new_with_finalizers.push(obj);
                continue;
            }

            marked.push(obj);
            pending.push(obj);

            while let Some(y) = pending.pop() {
                let state = self.finalization_state(y);

                if state == 0 {
                    self.bump_finalization_state_from_0_to_1(y);
                    let mut visitor = PendingVisitor {
                        local: self,
                        stack: &mut pending,
                    };

                    (*y).visit(&mut visitor);
                } else if state == 2 {

                    self.recursively_bump_finalization_state_from_2_to_3(y);
                }
            }
          
            self.recursively_bump_finalization_state_from_1_to_2(obj);
        }

        while let Some(x) = marked.pop() {
            let state = self.finalization_state(x);

            if state == 2 {
                (*self.pages).run_finalizers.push(x);
                self.recursively_bump_finalization_state_from_2_to_3(x);
            } else {
             
                new_with_finalizers.push(x);
            }
        }
        pending.clear();
        marked.clear();
        (*self.pages).finalizable = new_with_finalizers;
    }

    /// Exute "light finalizers" aka destructors. Always called right after marking
    unsafe fn execute_dtors(&mut self) {
        (*self.pages).destructible.retain(|object| {
            let object = *object;
            if (*object).is_visited() {
                true
            } else {
                match (*object).vtable().finalize {
                    Some(cb) => cb(object.add(1).cast()),
                    None => (),
                }
                false
            }
        });
    }
    /// Walk all destructible objects and keep them alive if they are still not initialized. We cannot destroy non initialized objects.
    unsafe fn check_uninitialized_dtors(&mut self) {
        /*for object in (*self.pages).destructible.iter().copied() {
            /*if !(*object).is_initialized() && !(*object).no_heap_ptrs() {
                self.mark_object(object);
            }*/
        }*/
    }
}

impl Visitor for LocalMarker {
    unsafe fn visit_conservative(&mut self, from: *mut u8, to: *mut u8) {
        let mut current = from as *mut *mut u8;
        let to = to as *mut *mut u8;
        while current < to {
            let addr = current.read();

            let header = (*self.pages).try_pointer_conservative(addr as _);
            if !header.is_null() {
                
                self.visit_pointer(header);
            }
            current = current.add(1);
        }
    }

    unsafe fn visit_pointer(&mut self, object: *mut ObjectHeader) {
        self.mark_object(object);
    }
}

unsafe impl Send for LocalMarker {}
pub struct GCMarker {
    marking_stack: MarkingStack,
    marked_bytes: usize,
    marked_micros: u64,
}

impl GCMarker {
    pub fn new() -> Self {
        Self {
            marking_stack: MarkingStack::new(),
            marked_bytes: 0,
            marked_micros: 0,
        }
    }
    pub const fn marked_bytes(&self) -> usize {
        self.marked_bytes
    }

    pub const fn marked_micros(&self) -> u64 {
        self.marked_micros
    }

    pub fn mark<T: Trace>(
        &mut self,
        pool: Option<(usize, &mut yastl::Pool)>,
        pages: *const Pages,
        roots: &mut T,
    ) {
        if let None = pool {
            let mut local = LocalMarker {
                pages: pages as *mut _,
                marking_worklist: MarkerWorkList::new(&self.marking_stack),
                marked_bytes: 0,
                mark_time: 0,

                num_busy: null_mut(),
                tmpstack: vec![],
            };

            let start = std::time::Instant::now();
            roots.trace(&mut local);

            unsafe {
                local.check_uninitialized_dtors();
                local.drain_marking_stack();
                local.deal_with_objects_with_finalizers();
                local.process_weak_refs();
                local.execute_dtors();
                local.drain_marking_stack();
            }

            local.mark_time = start.elapsed().as_micros() as u64;

            self.marked_bytes += local.marked_bytes;
            self.marked_micros += local.mark_time;
        } else if let Some((_num_tasks, _pool)) = pool {
            todo!()
        }
    }
}

pub struct FinalizationBumpVisitor<'a> {
    local: &'a mut LocalMarker,
}

impl Visitor for FinalizationBumpVisitor<'_> {
    unsafe fn visit_conservative(&mut self, from: *mut u8, to: *mut u8) {
        let mut current = from as *mut *mut u8;
        let to = to as *mut *mut u8;
        while current < to {
            let addr = current.read();
            let header = (*self.local.pages).try_pointer_conservative(addr as _);
            if !header.is_null() {
                self.visit_pointer(header);
            }
            current = current.add(1);
        }
    }

    unsafe fn visit_pointer(&mut self, object: *mut ObjectHeader) {
        self.local.tmpstack.push(object);
    }
}
