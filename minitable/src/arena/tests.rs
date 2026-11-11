use crate::__private::RowId;
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Id(pub u8, pub u64);

impl RowId for Id {
    fn from_parts(index: usize, generation: u64) -> Option<Self> {
        Some(Self(index.try_into().ok()?, generation))
    }
    fn index(self) -> usize {
        usize::from(self.0)
    }
    fn generation(self) -> u64 {
        self.1
    }
}

#[derive(Debug)]
struct Probe {
    index: usize,
    drops: Rc<Vec<Cell<usize>>>,
    panic: bool,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.drops[self.index].set(self.drops[self.index].get() + 1);
        assert!(!self.panic, "test destructor panic");
    }
}

fn counters(count: usize) -> Rc<Vec<Cell<usize>>> {
    Rc::new((0..count).map(|_| Cell::new(0)).collect())
}

macro_rules! arena_tests {
    ($implementation:ident, $generational:literal) => {
        mod $implementation {
            use super::*;
            type Arena<T, Id> = crate::arena::Arena<T, Id, $generational>;

            #[test]
            fn reuse_and_sparse_iteration_across_bitmap_words() {
                let mut arena = Arena::<_, Id>::default();
                assert!(arena.is_empty());
                let ids: Vec<_> = (0..256).map(|value| arena.insert(value).unwrap()).collect();
                for i in (0..256).step_by(2) {
                    assert_eq!(arena.remove(ids[i]), Some(i));
                    assert_eq!(arena.get(ids[i]), None);
                    assert_eq!(arena.get_mut(ids[i]), None);
                    assert_eq!(arena.remove(ids[i]), None);
                }
                assert_eq!(arena.iter().count(), 128);
                for (id, value) in arena.iter() {
                    assert_eq!(*value % 2, 1);
                    assert_eq!(id, ids[*value]);
                }
                let mut vacant: Vec<_> = (0..256).step_by(2).collect();
                vacant.reverse();
                for index in vacant {
                    let id = arena.insert(index + 256).unwrap();
                    assert_eq!(id.index(), index);
                    *arena.get_mut(id).unwrap() += 1;
                    assert_eq!(arena.get(id), Some(&(index + 257)));
                }
                assert_eq!(arena.len(), 256);
                assert_eq!(arena.iter().count(), 256);
            }

            #[test]
            fn values_are_dropped_once_after_removal_reuse_and_arena_drop() {
                let drops = counters(171);
                let mut arena = Arena::<_, Id>::default();
                let ids: Vec<_> = (0..128)
                    .map(|index| {
                        arena
                            .insert(Probe {
                                index,
                                drops: drops.clone(),
                                panic: false,
                            })
                            .unwrap()
                    })
                    .collect();
                for index in (0..128).step_by(3) {
                    let removed = arena.remove(ids[index]).unwrap();
                    assert_eq!(drops[index].get(), 0);
                    drop(removed);
                    assert_eq!(drops[index].get(), 1);
                }
                for index in 128..171 {
                    arena
                        .insert(Probe {
                            index,
                            drops: drops.clone(),
                            panic: false,
                        })
                        .unwrap();
                }
                drop(arena);
                assert!(drops.iter().all(|count| count.get() == 1));
            }

            #[test]
            fn capacity_error_returns_ownership_and_slot_can_be_reused() {
                let drops = counters(257);
                let mut arena = Arena::<_, Id>::default();
                let ids: Vec<_> = (0..256)
                    .map(|index| {
                        arena
                            .insert(Probe {
                                index,
                                drops: drops.clone(),
                                panic: false,
                            })
                            .unwrap()
                    })
                    .collect();
                assert!(!arena.can_insert());
                let rejected = arena
                    .insert(Probe {
                        index: 256,
                        drops: drops.clone(),
                        panic: false,
                    })
                    .err()
                    .unwrap();
                assert_eq!(rejected.index, 256);
                assert!(drops.iter().all(|count| count.get() == 0));
                drop(arena.remove(ids[0]).unwrap());
                assert!(arena.can_insert());
                assert_eq!(arena.insert(rejected).unwrap().index(), 0);
                drop(arena);
                assert!(drops.iter().all(|count| count.get() == 1));
            }

            #[test]
            fn destructor_panic_still_drops_remaining_values() {
                let drops = counters(8);
                let mut arena = Arena::<_, Id>::default();
                for index in 0..8 {
                    arena
                        .insert(Probe {
                            index,
                            drops: drops.clone(),
                            panic: index == 2,
                        })
                        .unwrap();
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(arena)));
                assert!(result.is_err());
                assert!(drops.iter().all(|count| count.get() == 1));
            }

            #[test]
            fn zero_sized_values_and_overaligned_values() {
                let mut arena = Arena::<(), Id>::default();
                let a = arena.insert(()).unwrap();
                let b = arena.insert(()).unwrap();
                assert_eq!(arena.remove(a), Some(()));
                let c = arena.insert(()).unwrap();
                assert_eq!(a.index(), c.index());
                assert_eq!(arena.iter().count(), 2);
                assert_eq!(arena.get(b), Some(&()));
                #[derive(Debug)]
                #[repr(align(128))]
                struct Aligned([u8; 3]);
                let mut arena = Arena::<_, Id>::default();
                let id = arena.insert(Aligned([1, 2, 3])).unwrap();
                assert_eq!(arena.get(id).unwrap().0, [1, 2, 3]);
                assert_eq!((arena.get(id).unwrap() as *const Aligned).addr() % 128, 0);
                assert_eq!(arena.remove(id).unwrap().0, [1, 2, 3]);
                arena.insert(Aligned([4, 5, 6])).unwrap();
            }
        }
    };
}

arena_tests!(generational, true);
arena_tests!(non_generational, false);

#[test]
fn non_generational_arena_keeps_generation_vector_empty() {
    let mut arena = super::Arena::<_, Id, false>::default();
    let ids: Vec<_> = (0..256).map(|value| arena.insert(value).unwrap()).collect();
    assert!(arena.generations.is_empty());
    assert_eq!(arena.generations.capacity(), 0);
    for id in ids {
        assert!(arena.get(id).is_some());
        assert!(arena.get_mut(id).is_some());
        assert!(arena.remove(id).is_some());
    }
    for value in 0..256 {
        arena.insert(value).unwrap();
    }
    assert_eq!(arena.iter().count(), 256);
    assert!(arena.generations.is_empty());
    assert_eq!(arena.generations.capacity(), 0);
}

#[test]
fn generations_reject_stale_mutable_access_after_reuse() {
    let mut arena = super::Arena::<_, Id, true>::default();
    let old = arena.insert(String::from("old")).unwrap();
    assert_eq!(arena.remove(old).unwrap(), "old");
    let new = arena.insert(String::from("new")).unwrap();
    assert_eq!(old.index(), new.index());
    assert_ne!(old.generation(), new.generation());
    assert!(arena.get(old).is_none());
    assert!(arena.get_mut(old).is_none());
    assert!(arena.remove(old).is_none());
    assert_eq!(arena.get(new).unwrap(), "new");
}

#[test]
fn generation_exhaustion_retires_slot() {
    let mut arena = super::Arena::<_, Id, true>::default();
    let first = arena.insert(1).unwrap();
    arena.generations[first.index()] = u64::MAX;
    let last = Id(first.0, u64::MAX);
    assert_eq!(arena.remove(last), Some(1));
    assert_eq!(arena.get(last), None);
    assert_eq!(arena.remove(last), None);
    let next = arena.insert(2).unwrap();
    assert_ne!(next.index(), first.index());
    assert_eq!(arena.len(), 1);
    assert_eq!(
        arena.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
        [2]
    );
}

#[test]
fn non_generational_arena_never_reads_id_generations() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct IndexOnly(usize);
    impl RowId for IndexOnly {
        fn from_parts(index: usize, generation: u64) -> Option<Self> {
            assert_eq!(generation, 0);
            Some(Self(index))
        }
        fn index(self) -> usize {
            self.0
        }
        fn generation(self) -> u64 {
            panic!("non-generational arena read a generation")
        }
    }
    let mut arena = super::Arena::<_, IndexOnly, false>::default();
    assert!(arena.can_insert());
    let old = arena.insert(1).unwrap();
    assert_eq!(arena.get(old), Some(&1));
    *arena.get_mut(old).unwrap() = 2;
    assert_eq!(arena.remove(old), Some(2));
    let new = arena.insert(3).unwrap();
    assert_eq!(new, old);
    assert_eq!(arena.iter().collect::<Vec<_>>(), [(new, &3)]);
    assert_eq!(arena.remove(new), Some(3));
    assert!(arena.generations.is_empty());
    assert_eq!(arena.generations.capacity(), 0);
}
