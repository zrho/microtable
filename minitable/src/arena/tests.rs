use super::Arena;
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Id(pub u8);

fn from_index(index: usize) -> Option<Id> {
    Some(Id(index.try_into().ok()?))
}

fn to_index(id: Id) -> Option<usize> {
    Some(usize::from(id.0))
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

#[test]
fn reuse_and_sparse_iteration_across_bitmap_words() {
    let mut arena = Arena::new(from_index, to_index);
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
        assert_eq!(to_index(id), Some(index));
        *arena.get_mut(id).unwrap() += 1;
        assert_eq!(arena.get(id), Some(&(index + 257)));
    }
    assert_eq!(arena.len(), 256);
    assert_eq!(arena.iter().count(), 256);
}

#[test]
fn values_are_dropped_once_after_removal_reuse_and_arena_drop() {
    let drops = counters(171);
    let mut arena = Arena::new(from_index, to_index);
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
    let mut arena = Arena::new(from_index, to_index);
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
    assert_eq!(to_index(arena.insert(rejected).unwrap()), Some(0));
    drop(arena);
    assert!(drops.iter().all(|count| count.get() == 1));
}

#[test]
fn destructor_panic_still_drops_remaining_values() {
    let drops = counters(8);
    let mut arena = Arena::new(from_index, to_index);
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
    let mut arena = Arena::<(), Id>::new(from_index, to_index);
    let a = arena.insert(()).unwrap();
    let b = arena.insert(()).unwrap();
    assert_eq!(arena.remove(a), Some(()));
    let c = arena.insert(()).unwrap();
    assert_eq!(to_index(a), to_index(c));
    assert_eq!(arena.iter().count(), 2);
    assert_eq!(arena.get(b), Some(&()));
    #[derive(Debug)]
    #[repr(align(128))]
    struct Aligned([u8; 3]);
    let mut arena = Arena::new(from_index, to_index);
    let id = arena.insert(Aligned([1, 2, 3])).unwrap();
    assert_eq!(arena.get(id).unwrap().0, [1, 2, 3]);
    assert_eq!((arena.get(id).unwrap() as *const Aligned).addr() % 128, 0);
    assert_eq!(arena.remove(id).unwrap().0, [1, 2, 3]);
    arena.insert(Aligned([4, 5, 6])).unwrap();
}
