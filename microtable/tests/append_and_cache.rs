use std::cell::Cell;
use std::rc::Rc;

#[derive(Default)]
struct CollisionHasher;
impl std::hash::Hasher for CollisionHasher {
    fn finish(&self) -> u64 {
        0
    }
    fn write(&mut self, _: &[u8]) {}
}
type CollisionBuilder = std::hash::BuildHasherDefault<CollisionHasher>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Empty,
    Tuple(u32, String),
    Named { number: u32, group: String },
}
impl Event {
    fn parts(&self) -> Option<(u32, &str)> {
        match self {
            Self::Empty => None,
            Self::Tuple(number, group) | Self::Named { number, group } => Some((*number, group)),
        }
    }
}
microtable::table! {
    #[table(append_only, hasher = CollisionBuilder)]
    #[derive(Debug, Clone)]
    struct Events([Event]);
    #[id]
    struct EventId(u16);
    #[index(unique, optional)]
    fn number(row: &Event) -> Option<u32> { row.parts().map(|(number, _)| number) }
    #[index(unique, optional, cached)]
    fn cached_number(row: &Event) -> Option<u32> { row.parts().map(|(number, _)| number) }
    #[index(many, optional)]
    fn group(row: &Event) -> Option<&str> { row.parts().map(|(_, group)| group) }
    #[index(many, optional, cached)]
    fn cached_group(row: &Event) -> Option<String> { row.parts().map(|(_, group)| group.to_owned()) }
}

#[test]
fn append_enum_collisions_growth_and_optional_circular_lists() {
    let mut table = Events::default();
    assert!(table.is_empty());
    assert_eq!(table.iter().count(), 0);
    table.insert(Event::Empty).unwrap();
    table.insert(Event::Empty).unwrap();
    for number in 0..320 {
        let group = ["red", "blue", "green"][number as usize % 3].to_owned();
        let row = if number % 2 == 0 {
            Event::Tuple(number, group)
        } else {
            Event::Named { number, group }
        };
        let predicted = table.next_id();
        assert_eq!(table.next_id(), predicted);
        let id = table.insert(row.clone()).unwrap();
        assert_eq!(id, predicted);
        assert_eq!(id.index(), number as usize + 2);
        assert_eq!(table.get(id), Some(&row));
    }
    assert_eq!(table.len(), 322);
    for number in 0..320 {
        assert_eq!(
            table.get_by_number(&number).unwrap().0.index(),
            number as usize + 2
        );
        assert_eq!(
            table.get_by_cached_number(&number).unwrap().0.index(),
            number as usize + 2
        );
    }
    let next = table.next_id();
    assert_eq!(
        table
            .insert(Event::Tuple(0, "rejected".into()))
            .unwrap_err()
            .1
            .index(),
        2
    );
    assert_eq!(table.next_id(), next);
    assert_eq!(table.iter_by_group("rejected").count(), 0);
    assert_eq!(table.iter_by_cached_group("rejected").count(), 0);
    for (remainder, group) in ["red", "blue", "green"].into_iter().enumerate() {
        for mut iter in [
            table
                .iter_by_group(group)
                .take(321)
                .collect::<Vec<_>>()
                .into_iter(),
            table
                .iter_by_cached_group(group)
                .take(321)
                .collect::<Vec<_>>()
                .into_iter(),
        ] {
            let mut numbers = Vec::new();
            for _ in 0..321 {
                let Some((id, row)) = iter.next() else {
                    break;
                };
                assert_eq!(table.get(id), Some(row));
                numbers.push(row.parts().unwrap().0);
            }
            assert!(iter.next().is_none());
            assert!(iter.next().is_none());
            numbers.sort_unstable();
            assert_eq!(
                numbers,
                (0..320)
                    .filter(|n| *n as usize % 3 == remainder)
                    .collect::<Vec<_>>()
            );
        }
    }
    // Exercise the generated iterators directly, including their ring stop condition.
    let mut live = table.iter_by_group("red");
    let mut cached = table.iter_by_cached_group("red");
    for _ in 0..107 {
        assert!(live.next().is_some());
        assert!(cached.next().is_some());
    }
    assert!(live.next().is_none());
    assert!(live.next().is_none());
    assert!(cached.next().is_none());
    assert!(cached.next().is_none());
    let matches = {
        let query = String::from("blue");
        table.iter_by_group(query.as_str())
    };
    assert_eq!(matches.take(321).count(), 107);
    let cached_matches = {
        let query = String::from("blue");
        table.iter_by_cached_group(query.as_str())
    };
    assert_eq!(cached_matches.take(321).count(), 107);
    let singleton = table
        .insert(Event::Named {
            number: 320,
            group: "singleton".into(),
        })
        .unwrap();
    let mut iter = table.iter_by_cached_group("singleton");
    assert_eq!(iter.next().unwrap().0, singleton);
    assert!(iter.next().is_none());
    assert!(iter.next().is_none());
}

#[test]
fn append_clone_and_debug_keep_independent_ring_storage() {
    let mut table = Events::new();
    let a = table.insert(Event::Tuple(1, "red".into())).unwrap();
    let mut cloned = table.clone();
    assert!(!format!("{cloned:?}").is_empty());
    assert_eq!(cloned.get_by_number(&1).unwrap().0, a);
    cloned
        .insert(Event::Named {
            number: 2,
            group: "red".into(),
        })
        .unwrap();
    assert_eq!(table.iter_by_group("red").count(), 1);
    assert_eq!(cloned.iter_by_group("red").count(), 2);
    assert_eq!(cloned.iter_by_cached_group("red").count(), 2);
    assert_eq!(table.len(), 1);
}

// Cached extractor outputs are moved, so they need not implement Clone.
#[derive(Debug, PartialEq, Eq, Hash)]
struct OwnedKey(u32);
impl std::borrow::Borrow<u32> for OwnedKey {
    fn borrow(&self) -> &u32 {
        &self.0
    }
}
#[derive(Debug)]
struct Counted {
    number: u32,
    calls: Rc<Cell<usize>>,
}
fn counted(row: &Counted) -> OwnedKey {
    row.calls.set(row.calls.get() + 1);
    OwnedKey(row.number)
}
microtable::table! {
    #[table(hasher = CollisionBuilder)]
    struct CachedRows([Counted]);
    #[index(unique, cached)]
    fn key(row: &Counted) -> OwnedKey { counted(row) }
    #[index(many, cached)]
    fn group(row: &Counted) -> OwnedKey { counted(row) }
}
microtable::table! {
    #[table(append_only, hasher = CollisionBuilder)]
    struct AppendCachedRows([Counted]);
    #[index(unique, cached)]
    fn key(row: &Counted) -> OwnedKey { counted(row) }
    #[index(many, cached)]
    fn group(row: &Counted) -> OwnedKey { counted(row) }
}

#[test]
fn cached_extractors_are_not_recomputed_for_reads_rehash_or_removal() {
    let calls = Rc::new(Cell::new(0));
    let mut table = CachedRows::new();
    let mut ids = Vec::new();
    for number in 0..128 {
        ids.push(
            table
                .insert(Counted {
                    number,
                    calls: calls.clone(),
                })
                .unwrap(),
        );
        assert_eq!(calls.get(), 2 * (number as usize + 1));
        assert_eq!(table.get_by_key(&number).unwrap().0, ids[number as usize]);
        assert_eq!(table.iter_by_group(&number).count(), 1);
        assert_eq!(calls.get(), 2 * (number as usize + 1));
    }
    table
        .replace(
            ids[0],
            Counted {
                number: 128,
                calls: calls.clone(),
            },
        )
        .unwrap();
    assert_eq!(calls.get(), 258);
    assert!(table.get_by_key(&0).is_none());
    assert_eq!(table.get_by_key(&128).unwrap().0, ids[0]);
    table
        .replace(
            ids[0],
            Counted {
                number: 128,
                calls: calls.clone(),
            },
        )
        .unwrap();
    assert_eq!(calls.get(), 260);
    for id in ids {
        table.remove(id).unwrap();
    }
    assert_eq!(calls.get(), 260);
    assert!(table.is_empty());
    table
        .replace(
            0,
            Counted {
                number: 999,
                calls: calls.clone(),
            },
        )
        .unwrap_err();
    assert_eq!(calls.get(), 260);
}

#[test]
fn append_cached_extractors_run_once_per_insert_not_per_query_or_growth() {
    let calls = Rc::new(Cell::new(0));
    let mut table = AppendCachedRows::new();
    for number in 0..128 {
        let id = table
            .insert(Counted {
                number,
                calls: calls.clone(),
            })
            .unwrap();
        assert_eq!(calls.get(), 2 * (number as usize + 1));
        assert_eq!(table.get_by_key(&number).unwrap().0, id);
        assert_eq!(table.iter_by_group(&number).count(), 1);
        assert_eq!(calls.get(), 2 * (number as usize + 1));
    }
    assert_eq!(table.len(), 128);
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct NoClone(u32);
#[derive(Debug)]
struct LiveRow {
    key: NoClone,
    group: NoClone,
}
microtable::table! {
    struct LiveRows([LiveRow]);
    #[index(unique)]
    fn key(row: &LiveRow) -> &NoClone { &row.key }
    #[index(many)]
    fn group(row: &LiveRow) -> &NoClone { &row.group }
}
#[test]
fn default_noncached_row_borrowed_keys_need_no_clone() {
    let mut table = LiveRows::new();
    let id = table
        .insert(LiveRow {
            key: NoClone(1),
            group: NoClone(2),
        })
        .unwrap();
    assert_eq!(table.get_by_key(&NoClone(1)).unwrap().0, id);
    assert_eq!(table.iter_by_group(&NoClone(2)).next().unwrap().0, id);
    table
        .replace(
            id,
            LiveRow {
                key: NoClone(3),
                group: NoClone(4),
            },
        )
        .unwrap();
    assert!(table.get_by_key(&NoClone(1)).is_none());
    assert_eq!(table.iter_by_group(&NoClone(2)).count(), 0);
    table.remove(id).unwrap();
    assert!(table.get_by_key(&NoClone(3)).is_none());
}
