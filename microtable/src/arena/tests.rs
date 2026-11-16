use super::{AppendArena, Arena};
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Id(pub usize);

fn from_index(index: usize) -> Id {
    Id(index ^ 0x5a5a)
}

fn to_index(id: Id) -> usize {
    id.0 ^ 0x5a5a
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
    let ids: Vec<_> = (0..1024)
        .map(|value| arena.insert(value).unwrap())
        .collect();
    assert!(arena.can_insert());
    assert_eq!(arena.get(from_index(usize::MAX)), None);
    assert_eq!(arena.get_mut(from_index(usize::MAX)), None);
    assert_eq!(arena.remove(from_index(usize::MAX)), None);
    for i in (0..1024).step_by(2) {
        assert_eq!(arena.remove(ids[i]), Some(i));
        assert_eq!(arena.get(ids[i]), None);
        assert_eq!(arena.get_mut(ids[i]), None);
        assert_eq!(arena.remove(ids[i]), None);
    }
    assert_eq!(arena.iter().count(), 512);
    for (id, value) in arena.iter() {
        assert_eq!(*value % 2, 1);
        assert_eq!(id, ids[*value]);
    }
    let mut vacant: Vec<_> = (0..1024).step_by(2).collect();
    vacant.reverse();
    for index in vacant {
        let id = arena.insert(index + 1024).unwrap();
        assert_eq!(to_index(id), index);
        *arena.get_mut(id).unwrap() += 1;
        assert_eq!(arena.get(id), Some(&(index + 1025)));
    }
    assert_eq!(arena.len(), 1024);
    assert_eq!(arena.iter().count(), 1024);

    let mut append = AppendArena::new(from_index, to_index);
    assert!(append.is_empty());
    for index in 0..1024 {
        assert!(append.can_insert());
        let id = append.next_id();
        assert_eq!(to_index(id), index);
        append.reserve(1);
        append.push(index);
        assert_eq!(append.get(id), Some(&index));
        *append.get_mut(id).unwrap() += 1;
    }
    assert_eq!(append.len(), 1024);
    assert_eq!(append.get(from_index(1024)), None);
    assert_eq!(append.get_mut(from_index(usize::MAX)), None);
    for (index, (id, value)) in append.iter().enumerate() {
        assert_eq!(to_index(id), index);
        assert_eq!(*value, index + 1);
    }
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
fn growth_preserves_ownership_and_slot_can_be_reused() {
    let drops = counters(1025);
    let mut arena = Arena::new(from_index, to_index);
    let ids: Vec<_> = (0..1024)
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
    assert!(arena.can_insert());
    let owned = arena.remove(ids[0]).unwrap();
    assert_eq!(owned.index, 0);
    assert!(drops.iter().all(|count| count.get() == 0));
    assert!(arena.can_insert());
    assert_eq!(to_index(arena.insert(owned).unwrap()), 0);
    let id = arena
        .insert(Probe {
            index: 1024,
            drops: drops.clone(),
            panic: false,
        })
        .unwrap();
    assert_eq!(to_index(id), 1024);
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
    let mut append = AppendArena::<(), Id>::new(from_index, to_index);
    for index in 0..1024 {
        assert!(append.can_insert());
        assert_eq!(to_index(append.next_id()), index);
        append.reserve(1);
        append.push(());
    }
    assert_eq!(append.len(), 1024);
    assert_eq!(append.get(from_index(1023)), Some(&()));
    assert_eq!(append.get(from_index(1024)), None);
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
