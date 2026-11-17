use microtable::ReplaceErrorKind;

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
struct Row {
    serial: u32,
    live_unique: Option<u32>,
    cached_unique: Option<u32>,
    live_many: Option<u32>,
    cached_many: Option<u32>,
}
impl Row {
    fn new(serial: u32, unique: Option<u32>, group: Option<u32>) -> Self {
        Self {
            serial,
            live_unique: unique,
            cached_unique: unique,
            live_many: group,
            cached_many: group,
        }
    }
}
microtable::table! {
    #[table(hasher = CollisionBuilder)]
    #[derive(Debug, Clone)]
    struct Rows([Row]);
    #[index(unique, optional)]
    fn live_unique(row: &Row) -> Option<u32> { row.live_unique }
    #[index(unique, optional, cached)]
    fn cached_unique(row: &Row) -> Option<u32> { row.cached_unique }
    #[index(many, optional)]
    fn live_many(row: &Row) -> Option<u32> { row.live_many }
    #[index(many, optional, cached)]
    fn cached_many(row: &Row) -> Option<u32> { row.cached_many }
    // Without `optional`, None is a real key, not a missing index entry.
    #[index(many)]
    fn option_key(row: &Row) -> Option<u32> { row.live_many }
    #[index(many, cached)]
    fn cached_option_key(row: &Row) -> Option<u32> { row.cached_many }
}

fn check(table: &Rows) {
    for key in 0..400 {
        for (actual, expected) in [
            (
                table.get_by_live_unique(&key).map(|(id, _)| id),
                table
                    .iter()
                    .find(|(_, row)| row.live_unique == Some(key))
                    .map(|(id, _)| id),
            ),
            (
                table.get_by_cached_unique(&key).map(|(id, _)| id),
                table
                    .iter()
                    .find(|(_, row)| row.cached_unique == Some(key))
                    .map(|(id, _)| id),
            ),
        ] {
            assert_eq!(actual, expected);
        }
        for (actual, expected) in [
            (
                table
                    .iter_by_live_many(&key)
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>(),
                table
                    .iter()
                    .filter(|(_, row)| row.live_many == Some(key))
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>(),
            ),
            (
                table.iter_by_cached_many(&key).map(|(id, _)| id).collect(),
                table
                    .iter()
                    .filter(|(_, row)| row.cached_many == Some(key))
                    .map(|(id, _)| id)
                    .collect(),
            ),
        ] {
            let mut actual = actual;
            let mut expected = expected;
            actual.sort_unstable();
            expected.sort_unstable();
            assert_eq!(actual, expected);
        }
    }
    for key in [None, Some(0), Some(1), Some(2)] {
        for (actual, expected) in [
            (
                table
                    .iter_by_option_key(&key)
                    .map(|(_, row)| row.serial)
                    .collect::<Vec<_>>(),
                table
                    .iter()
                    .filter(|(_, row)| row.live_many == key)
                    .map(|(_, row)| row.serial)
                    .collect::<Vec<_>>(),
            ),
            (
                table
                    .iter_by_cached_option_key(&key)
                    .map(|(_, row)| row.serial)
                    .collect(),
                table
                    .iter()
                    .filter(|(_, row)| row.cached_many == key)
                    .map(|(_, row)| row.serial)
                    .collect(),
            ),
        ] {
            let mut actual = actual;
            let mut expected = expected;
            actual.sort_unstable();
            expected.sort_unstable();
            assert_eq!(actual, expected);
        }
    }
}

#[test]
fn absent_optional_keys_can_repeat_but_plain_option_keys_index_none() {
    let mut table = Rows::new();
    for serial in 0..4 {
        table.insert(Row::new(serial, None, None)).unwrap();
    }
    assert_eq!(table.iter_by_option_key(&None).count(), 4);
    assert_eq!(table.iter_by_cached_option_key(&None).count(), 4);
    assert!(table.get_by_live_unique(&0).is_none());
    assert!(table.get_by_cached_unique(&0).is_none());
    assert_eq!(table.iter_by_live_many(&0).count(), 0);
    assert_eq!(table.iter_by_cached_many(&0).count(), 0);
    check(&table);
}

#[test]
fn all_optional_replace_transitions_for_both_storage_modes() {
    let mut table = Rows::new();
    let id = table.insert(Row::new(0, None, None)).unwrap();
    // None->None, None->Some, same Some, changed Some, Some->None.
    for (serial, key, group) in [
        (1, None, None),
        (2, Some(10), Some(0)),
        (3, Some(10), Some(0)),
        (4, Some(11), Some(1)),
        (5, None, None),
        (6, Some(12), Some(2)),
    ] {
        let old = table.get(id).unwrap().clone();
        assert_eq!(
            table.replace(id, Row::new(serial, key, group)).unwrap(),
            old
        );
        assert_eq!(table.len(), 1);
        check(&table);
    }
    assert_eq!(table.remove(id), Some(Row::new(6, Some(12), Some(2))));
    check(&table);
    assert!(table.is_empty());
}

#[test]
fn conflicts_in_each_optional_unique_index_leave_all_links_unchanged() {
    let mut table = Rows::new();
    let a = table.insert(Row::new(1, Some(10), Some(0))).unwrap();
    let b = table.insert(Row::new(2, Some(20), Some(0))).unwrap();
    for (live_unique, cached_unique, field) in [
        (Some(10), Some(30), "live_unique"),
        (Some(30), Some(10), "cached_unique"),
        (None, Some(10), "cached_unique"),
        (Some(10), None, "live_unique"),
    ] {
        let row = Row {
            serial: 3,
            live_unique,
            cached_unique,
            live_many: Some(1),
            cached_many: None,
        };
        let next = table.next_id();
        assert_eq!(table.insert(row.clone()).unwrap_err(), (row.clone(), a));
        assert_eq!(table.next_id(), next);
        let error = table.replace(b, row.clone()).unwrap_err();
        assert_eq!(error.kind, ReplaceErrorKind::Unique { field });
        assert_eq!(error.row, row);
        assert_eq!(table.get(b), Some(&Row::new(2, Some(20), Some(0))));
        check(&table);
    }
    // Keeping one's own keys is not a conflict, even when changing group presence.
    table.replace(a, Row::new(4, Some(10), None)).unwrap();
    check(&table);
}

#[test]
fn optional_groups_promote_representatives_across_removal_and_replacement() {
    let mut table = Rows::new();
    for serial in 0..8 {
        table
            .insert(Row::new(serial, Some(serial), Some(0)))
            .unwrap();
    }
    let head = table.iter_by_live_many(&0).next().unwrap().0;
    table.replace(head, Row::new(20, None, None)).unwrap();
    check(&table);
    let cached_head = table.iter_by_cached_many(&0).next().unwrap().0;
    table.remove(cached_head).unwrap();
    check(&table);
    let ids: Vec<_> = table.iter_by_live_many(&0).map(|(id, _)| id).collect();
    for id in ids {
        let serial = table.get(id).unwrap().serial;
        table.replace(id, Row::new(serial, None, Some(1))).unwrap();
        check(&table);
    }
    assert_eq!(table.iter_by_live_many(&0).count(), 0);
    assert_eq!(table.iter_by_cached_many(&0).count(), 0);
    let ids: Vec<_> = table.iter().map(|(id, _)| id).collect();
    for id in ids {
        table.remove(id).unwrap();
        check(&table);
    }
    let id = table.insert(Row::new(99, Some(99), Some(0))).unwrap();
    assert_eq!(table.iter_by_live_many(&0).next().unwrap().0, id);
    assert_eq!(table.iter_by_cached_many(&0).next().unwrap().0, id);
}

#[test]
fn optional_collision_rehash_preserves_present_and_absent_entries() {
    let mut table = Rows::new();
    for serial in 0..320 {
        table
            .insert(Row::new(
                serial,
                (serial % 3 != 0).then_some(serial),
                (serial % 4 != 0).then_some(serial % 3),
            ))
            .unwrap();
    }
    check(&table);
    let ids: Vec<_> = table
        .iter()
        .filter(|(_, row)| row.serial % 5 == 0)
        .map(|(id, _)| id)
        .collect();
    for id in ids {
        table.remove(id).unwrap();
    }
    check(&table);
    let ids: Vec<_> = table.iter().map(|(id, _)| id).collect();
    for id in ids {
        let serial = table.get(id).unwrap().serial;
        table
            .replace(
                id,
                Row::new(
                    serial,
                    serial.is_multiple_of(3).then_some(serial),
                    serial.is_multiple_of(4).then_some(serial % 3),
                ),
            )
            .unwrap();
    }
    check(&table);
    let cloned = table.clone();
    check(&cloned);
}

#[derive(Debug)]
struct PlainOption(Option<u32>);
microtable::table! {
    struct PlainOptions([PlainOption]);
    #[index(unique)]
    fn key(row: &PlainOption) -> Option<u32> { row.0 }
}
#[test]
fn plain_option_unique_none_conflicts_like_any_other_key() {
    let mut table = PlainOptions::new();
    let id = table.insert(PlainOption(None)).unwrap();
    assert_eq!(table.insert(PlainOption(None)).unwrap_err().1, id);
    assert_eq!(table.get_by_key(&None).unwrap().0, id);
    table.replace(id, PlainOption(Some(1))).unwrap();
    assert!(table.get_by_key(&None).is_none());
    table.insert(PlainOption(None)).unwrap();
    assert_eq!(table.len(), 2);
}
