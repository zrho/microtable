use microtable::{ReplaceErrorKind, table};

type CollisionBuilder = std::hash::BuildHasherDefault<CollisionHasher>;
type StableBuilder = std::hash::BuildHasherDefault<std::collections::hash_map::DefaultHasher>;

// Force distinct keys into the same bucket in mutable and append-only indexes.
#[derive(Default)]
struct CollisionHasher;

impl std::hash::Hasher for CollisionHasher {
    fn finish(&self) -> u64 {
        0
    }

    fn write(&mut self, _: &[u8]) {}
}

#[table(hasher = std::hash::BuildHasherDefault<CollisionHasher>)]
#[derive(Debug, PartialEq, Eq)]
pub struct Person {
    #[indexed(unique)]
    email: String,
    #[indexed(unique)]
    number: u32,
    #[indexed(many)]
    team: String,
    #[indexed(many, cached)]
    active: bool,
    payload: Vec<u8>,
}

fn person(number: u32, team: &str) -> Person {
    Person {
        email: format!("{number}@example.com"),
        number,
        team: team.into(),
        active: number.is_multiple_of(2),
        payload: vec![],
    }
}

fn members(table: &PersonTable, team: &str) -> Vec<u32> {
    let mut result: Vec<_> = table
        .iter_by_team(team)
        .map(|(_, row)| row.number)
        .collect();
    result.sort_unstable();
    result
}

#[test]
fn insert_unique_checks_and_borrowed_lookup() {
    let mut table = PersonTable::default();
    assert!(table.is_empty());
    let predicted = table.next_id();
    assert_eq!(table.next_id(), predicted);
    assert!(!table.contains(predicted));
    assert!(table.is_empty());
    let id = table.insert(person(1, "red")).unwrap();
    assert_eq!(id, predicted);
    assert_eq!(table.get_by_email("1@example.com").unwrap().0, id);
    assert_eq!(table.get_by_number(&1).unwrap().0, id);
    let mut rejected = person(1, "blue");
    rejected.email = "other@example.com".into();
    let predicted = table.next_id();
    let error = table.insert(rejected).unwrap_err();
    assert_eq!(table.next_id(), predicted);
    assert_eq!(error.1, id);
    assert_eq!(error.0.email, "other@example.com");
    assert_eq!(error.0.team, "blue");
    assert!(table.get_by_email("other@example.com").is_none());
    assert_eq!(members(&table, "red"), [1]);
    assert!(members(&table, "blue").is_empty());
    assert_eq!(table.len(), 1);
}

#[derive(Debug)]
struct CountedKey {
    value: u32,
    hashes: std::rc::Rc<std::cell::Cell<usize>>,
    clones: std::rc::Rc<std::cell::Cell<usize>>,
}

impl CountedKey {
    fn new(value: u32) -> Self {
        Self {
            value,
            hashes: Default::default(),
            clones: Default::default(),
        }
    }
}

impl Clone for CountedKey {
    fn clone(&self) -> Self {
        self.clones.set(self.clones.get() + 1);
        Self {
            value: self.value,
            hashes: self.hashes.clone(),
            clones: self.clones.clone(),
        }
    }
}

impl std::borrow::Borrow<u32> for CountedKey {
    fn borrow(&self) -> &u32 {
        &self.value
    }
}

impl PartialEq for CountedKey {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}
impl Eq for CountedKey {}

impl std::hash::Hash for CountedKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.hashes.set(self.hashes.get() + 1);
        self.value.hash(state);
    }
}

macro_rules! counted_row {
    ($name:ident $(, $option:meta)*) => {
        #[table($($option),*)]
        #[derive(Debug)]
        struct $name {
            #[indexed(unique)]
            first: CountedKey,
            #[indexed(unique, cached)]
            second: CountedKey,
            #[indexed(many)]
            group: u8,
        }

        impl $name {
            fn new(first: u32, second: u32, group: u8) -> Self {
                Self {
                    first: CountedKey::new(first),
                    second: CountedKey::new(second),
                    group,
                }
            }
        }
    };
}

counted_row!(CountedRow);
counted_row!(AppendCountedRow, append_only);

#[test]
fn unique_entries_hash_each_new_key_once() {
    let mut table = CountedRowTable::new();
    // Use nonempty indexes, with room for this insert without a rehash.
    table
        .insert(CountedRow {
            first: CountedKey::new(0),
            second: CountedKey::new(0),
            group: 0,
        })
        .unwrap();
    let row = CountedRow {
        first: CountedKey::new(1),
        second: CountedKey::new(1),
        group: 1,
    };
    let first_hashes = row.first.hashes.clone();
    let second_hashes = row.second.hashes.clone();
    let first_clones = row.first.clones.clone();
    let second_clones = row.second.clones.clone();
    let id = table.insert(row).unwrap();
    assert_eq!(first_hashes.get(), 1);
    assert_eq!(second_hashes.get(), 1);
    assert_eq!(first_clones.get(), 0); // Default field keys stay in the row.
    assert_eq!(second_clones.get(), 1); // Cached fields own one key.

    let mut append = AppendCountedRowTable::new();
    append.insert(AppendCountedRow::new(0, 0, 0)).unwrap();
    let row = AppendCountedRow::new(1, 1, 0);
    let counters = [&row.first, &row.second].map(|key| (key.hashes.clone(), key.clones.clone()));
    let append_id = append.insert(row).unwrap();
    for (index, (hashes, clones)) in counters.into_iter().enumerate() {
        assert_eq!(hashes.get(), 1);
        assert_eq!(clones.get(), index);
    }
    assert_eq!(append.get_by_first(&1).unwrap().0, append_id);
    assert_eq!(append.get_by_second(&1).unwrap().0, append_id);
    assert_eq!(append.iter_by_group(&0).take(3).count(), 2);

    // Change one unique key and retain the other, with independent hash counters.
    let row = CountedRow {
        first: CountedKey::new(2),
        second: CountedKey::new(1),
        group: 2,
    };
    let new_first_hashes = row.first.hashes.clone();
    let new_second_hashes = row.second.hashes.clone();
    let new_first_clones = row.first.clones.clone();
    let new_second_clones = row.second.clones.clone();
    let old = table.replace(id, row).unwrap();
    assert_eq!(new_first_hashes.get(), 1);
    assert_eq!(new_second_hashes.get(), 1);
    assert_eq!(new_first_clones.get(), 0);
    assert_eq!(new_second_clones.get(), 0);
    // Row-backed stored keys may be hashed again during detach or rehash.
    assert_eq!(first_clones.get(), 0);
    assert_eq!(second_clones.get(), 1);
    assert_eq!(old.first.value, 1);
    assert_eq!(table.get(id).unwrap().first.value, 2);
    assert_eq!(table.iter_by_group(&1).count(), 0);
    assert_eq!(table.iter_by_group(&2).next().unwrap().0, id);
}

#[test]
fn conflicting_writes_do_not_clone_keys() {
    let mut table = CountedRowTable::new();
    let a = table.insert(CountedRow::new(0, 0, 0)).unwrap();
    let b = table.insert(CountedRow::new(1, 1, 0)).unwrap();
    for (first, second, field) in [(0, 2, "first"), (2, 0, "second")] {
        let row = CountedRow::new(first, second, 1);
        let clones = [&row.first, &row.second].map(|key| key.clones.clone());
        let error = table.insert(row).unwrap_err();
        assert_eq!(error.1, a);
        assert_eq!(error.0.first.value, first);
        assert_eq!(error.0.second.value, second);
        for clones in &clones {
            assert_eq!(clones.get(), 0);
            assert_eq!(std::rc::Rc::strong_count(clones), 2); // Rejected row is still owned.
        }
        drop(error);
        for clones in &clones {
            assert_eq!(std::rc::Rc::strong_count(clones), 1);
        }
        let error = table
            .replace(b, CountedRow::new(first, second, 1))
            .unwrap_err();
        assert_eq!(error.kind, ReplaceErrorKind::Unique { field });
        assert_eq!(error.row.first.clones.get(), 0);
        assert_eq!(error.row.second.clones.get(), 0);
        assert_eq!(table.len(), 2);
        assert_eq!(table.get_by_first(&0).unwrap().0, a);
        assert_eq!(table.get_by_first(&1).unwrap().0, b);
        assert!(table.get_by_first(&2).is_none());
        assert!(table.get_by_second(&2).is_none());
        assert_eq!(table.iter_by_group(&1).count(), 0);
    }
    let mut append = AppendCountedRowTable::new();
    let id = append.insert(AppendCountedRow::new(0, 0, 0)).unwrap();
    for (first, second) in [(0, 2), (2, 0)] {
        let row = AppendCountedRow::new(first, second, 1);
        let clones = [&row.first, &row.second].map(|key| key.clones.clone());
        let error = append.insert(row).unwrap_err();
        assert_eq!(error.1, id);
        assert_eq!(error.0.first.value, first);
        assert_eq!(error.0.second.value, second);
        for clones in &clones {
            assert_eq!(clones.get(), 0);
            assert_eq!(std::rc::Rc::strong_count(clones), 2);
        }
        drop(error);
        for clones in &clones {
            assert_eq!(std::rc::Rc::strong_count(clones), 1);
        }
        assert_eq!(append.len(), 1);
        assert_eq!(append.get_by_first(&0).unwrap().0, id);
        assert_eq!(append.get_by_second(&0).unwrap().0, id);
        assert!(append.get_by_first(&2).is_none());
        assert!(append.get_by_second(&2).is_none());
        assert!(append.iter_by_group(&1).next().is_none());
    }
    table.remove(b);
    let error = table.replace(b, CountedRow::new(2, 2, 1)).unwrap_err();
    assert_eq!(error.kind, ReplaceErrorKind::NotFound);
    assert_eq!(error.row.first.clones.get(), 0);
    assert_eq!(error.row.second.clones.get(), 0);
}

#[test]
fn growth_preserves_key_ownership() {
    let mut table = CountedRowTable::new();
    for value in 0..256 {
        table.insert(CountedRow::new(value, value, 0)).unwrap();
    }
    let row = CountedRow::new(256, 256, 1);
    let first_clones = row.first.clones.clone();
    let second_clones = row.second.clones.clone();
    let grown: usize = table.insert(row).unwrap();
    assert_eq!(grown, 256);
    assert_eq!(first_clones.get(), 0);
    assert_eq!(second_clones.get(), 1);
    assert_eq!(table.get_by_first(&256).unwrap().0, grown);
    assert_eq!(table.get_by_second(&256).unwrap().0, grown);
    assert_eq!(table.iter_by_group(&1).count(), 1);
    assert_eq!(table.len(), 257);
    // Growth preserves cached and row-backed key ownership in append-only tables.
    let mut append = AppendCountedRowTable::default();
    assert!(append.is_empty());
    for value in 0..256 {
        let id: usize = append
            .insert(AppendCountedRow::new(value, value, (value % 3) as u8))
            .unwrap();
        assert_eq!(id as u32, value);
        assert_eq!(append.get_by_first(&value).unwrap().0, id);
        assert_eq!(append.get_by_second(&value).unwrap().0, id);
    }
    let row = AppendCountedRow::new(256, 256, 3);
    let first_clones = row.first.clones.clone();
    let second_clones = row.second.clones.clone();
    let id: usize = append.insert(row).unwrap();
    assert_eq!(id, 256);
    assert_eq!(first_clones.get(), 0);
    assert_eq!(second_clones.get(), 1);
    assert_eq!(append.len(), 257);
    assert_eq!(append.iter().count(), 257);
    assert_eq!(append.get_by_first(&256).unwrap().0, id);
    assert_eq!(append.get_by_second(&256).unwrap().0, id);
    let mut singleton = append.iter_by_group(&3);
    assert_eq!(singleton.next().unwrap().0, id);
    assert!(singleton.next().is_none());
    assert!(singleton.next().is_none());
    for group in 0..3 {
        let mut actual: Vec<_> = append
            .iter_by_group(&group)
            .take(257)
            .map(|(_, row)| row.first.value)
            .collect();
        actual.sort_unstable();
        assert_eq!(
            actual,
            (0..256)
                .filter(|value| value % 3 == group as u32)
                .collect::<Vec<_>>()
        );
    }
    let first = table.get_by_first(&0).unwrap().0;
    table.remove(first);
    let row = table.remove(grown).unwrap();
    assert_eq!(row.first.clones.get(), 0);
    assert_eq!(row.second.clones.get(), 1);
    let reused = table.insert(row).unwrap();
    assert_eq!(reused, grown);
    assert_eq!(table.get_by_first(&256).unwrap().0, reused);
    assert!(!table.contains(first));
    assert_eq!(table.len(), 256);
}

#[table]
#[index(name = pair, unique, cached, fields(first, second))]
#[index(name = computed, unique, key_type = CountedKey, key = |row| counted_output(&row.derived))]
#[index(name = computed_group, many, key_type = CountedKey, key = |row| counted_output(&row.group))]
#[derive(Debug)]
struct NamedCountedRow {
    first: CountedKey,
    second: CountedKey,
    derived: CountedKey,
    group: CountedKey,
}

// Materialize a computed output without calling the instrumented key's Clone.
fn counted_output(key: &CountedKey) -> CountedKey {
    CountedKey {
        value: key.value,
        hashes: key.hashes.clone(),
        clones: key.clones.clone(),
    }
}

impl NamedCountedRow {
    fn new(first: u32, second: u32, derived: u32, group: u32) -> Self {
        Self {
            first: CountedKey::new(first),
            second: CountedKey::new(second),
            derived: CountedKey::new(derived),
            group: CountedKey::new(group),
        }
    }
}

#[test]
fn named_indexes_preserve_hash_and_clone_optimizations() {
    let mut table = NamedCountedRowTable::new();
    let row = NamedCountedRow::new(1, 2, 10, 5);
    let counters = [&row.first, &row.second, &row.derived, &row.group]
        .map(|key| (key.hashes.clone(), key.clones.clone()));
    let id = table.insert(row).unwrap();
    for (index, (hashes, clones)) in counters.into_iter().enumerate() {
        assert_eq!(hashes.get(), 1);
        // Cached composite fields clone once; computed outputs move into the cache.
        assert_eq!(clones.get(), usize::from(index < 2));
    }
    // A composite conflict and a later computed-key conflict both defer all cloning.
    for row in [
        NamedCountedRow::new(1, 2, 11, 6),
        NamedCountedRow::new(3, 4, 10, 6),
    ] {
        let clones =
            [&row.first, &row.second, &row.derived, &row.group].map(|key| key.clones.clone());
        let error = table.insert(row).unwrap_err();
        assert_eq!(error.1, id);
        assert_eq!(error.0.group.value, 6);
        for clones in &clones {
            assert_eq!(clones.get(), 0);
            assert_eq!(std::rc::Rc::strong_count(clones), 2);
        }
        drop(error);
        for clones in &clones {
            assert_eq!(std::rc::Rc::strong_count(clones), 1);
        }
        assert_eq!(table.len(), 1);
        assert_eq!(table.get_by_pair(&1, &2).unwrap().0, id);
        assert_eq!(table.get_by_computed(&10).unwrap().0, id);
        assert!(table.get_by_pair(&3, &4).is_none());
        assert!(table.get_by_computed(&11).is_none());
        assert_eq!(table.iter_by_computed_group(&5).count(), 1);
        assert_eq!(table.iter_by_computed_group(&6).count(), 0);
    }
    let row = NamedCountedRow::new(1, 2, 11, 6);
    let first_clones = row.first.clones.clone();
    let second_clones = row.second.clones.clone();
    let derived_clones = row.derived.clones.clone();
    let group_clones = row.group.clones.clone();
    table.replace(id, row).unwrap();
    assert_eq!(first_clones.get(), 0);
    assert_eq!(second_clones.get(), 0);
    assert_eq!(derived_clones.get(), 0);
    assert_eq!(group_clones.get(), 0);
    assert_eq!(table.get_by_pair(&1, &2).unwrap().0, id);
    assert!(table.get_by_pair(&3, &4).is_none());
    assert!(table.get_by_computed(&10).is_none());
    assert_eq!(table.get_by_computed(&11).unwrap().0, id);
    assert_eq!(table.iter_by_computed_group(&5).count(), 0);
    assert_eq!(table.iter_by_computed_group(&6).count(), 1);
    table.remove(id);
    assert!(table.get_by_pair(&1, &2).is_none());
    assert!(table.get_by_computed(&11).is_none());
    assert_eq!(table.iter_by_computed_group(&6).count(), 0);
}

#[test]
fn removal_keeps_ids_stable_and_reuses_slots() {
    let mut table = PersonTable::new();
    let a = table.insert(person(1, "red")).unwrap();
    let b = table.insert(person(2, "red")).unwrap();
    assert_eq!(table.remove(a), Some(person(1, "red")));
    assert!(!table.contains(a));
    assert_eq!(table.remove(a), None);
    let error = table.replace(a, person(4, "blue")).unwrap_err();
    assert_eq!(error.kind, ReplaceErrorKind::NotFound);
    let c = table.insert(person(3, "red")).unwrap();
    assert_eq!(a, c);
    assert_eq!(table.get(a).unwrap().number, 3);
    assert_eq!(table.get(b).unwrap().number, 2);
    assert_eq!(table.get(c).unwrap().number, 3);
    assert_eq!(table.len(), 2);
}

#[test]
fn intrusive_removal_handles_middle_tail_head_and_singleton() {
    let mut table = PersonTable::new();
    let a = table.insert(person(1, "red")).unwrap();
    let b = table.insert(person(2, "red")).unwrap();
    let c = table.insert(person(3, "red")).unwrap();
    let d = table.insert(person(4, "red")).unwrap();
    table.remove(b); // middle
    assert_eq!(members(&table, "red"), [1, 3, 4]);
    table.remove(a); // tail
    assert_eq!(members(&table, "red"), [3, 4]);
    table.remove(d); // head
    assert_eq!(members(&table, "red"), [3]);
    table.remove(c); // singleton
    assert!(members(&table, "red").is_empty());
    assert!(table.is_empty());
    table.insert(person(5, "red")).unwrap();
    assert_eq!(members(&table, "red"), [5]);
}

#[test]
fn replacement_updates_all_indexes_and_preserves_id() {
    let mut table = PersonTable::new();
    let a = table.insert(person(1, "red")).unwrap();
    let b = table.insert(person(2, "red")).unwrap();
    let c = table.insert(person(3, "blue")).unwrap();
    assert_eq!(
        table.replace(a, person(4, "blue")).unwrap(),
        person(1, "red")
    );
    assert_eq!(table.get_by_number(&4).unwrap().0, a);
    assert!(table.get_by_number(&1).is_none());
    assert!(table.get_by_email("1@example.com").is_none());
    assert_eq!(members(&table, "red"), [2]);
    assert_eq!(members(&table, "blue"), [3, 4]);
    let rejected = table.replace(b, person(4, "green")).unwrap_err();
    assert_eq!(rejected.kind, ReplaceErrorKind::Unique { field: "email" });
    assert_eq!(rejected.row, person(4, "green"));
    assert_eq!(table.get(b), Some(&person(2, "red")));
    assert!(members(&table, "green").is_empty());
    let mut rejected = person(4, "green");
    rejected.email = "unused@example.com".into();
    let error = table.replace(b, rejected).unwrap_err();
    assert_eq!(error.kind, ReplaceErrorKind::Unique { field: "number" });
    assert!(table.get_by_email("unused@example.com").is_none());
    assert_eq!(table.get_by_number(&2).unwrap().0, b);
    // Replacing with the same unique and many keys is allowed, including a list head.
    table.replace(c, person(3, "blue")).unwrap();
    table.replace(a, person(4, "blue")).unwrap();
    assert_eq!(members(&table, "blue"), [3, 4]);
    assert_eq!(table.iter_by_active(&true).count(), 2);
    table.remove(c);
    table.remove(a);
    assert!(members(&table, "blue").is_empty());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TinyId(usize);

impl TinyId {
    fn new(index: usize) -> Self {
        Self(index ^ usize::MAX)
    }

    fn index(self) -> usize {
        self.0 ^ usize::MAX
    }
}

macro_rules! tiny_row {
    ($name:ident, $table:ident $(, $option:meta)*) => {
        #[table(
            table = $table,
            id = TinyId,
            id_from_index = TinyId::new,
            id_to_index = |id| id.index(),
            $($option,)*
        )]
        #[derive(Debug)]
        struct $name {
            #[indexed(many)]
            group: u8,
        }
    };
}

tiny_row!(Tiny, TinyTable);
tiny_row!(AppendTiny, AppendTinyTable, append_only = true);

macro_rules! tiny_enum {
    ($row:ident $(, $option:meta)*) => {
        #[table(
            id = TinyId,
            id_from_index = TinyId::new,
            id_to_index = |id| id.index(),
            $($option,)*
        )]
        #[derive(Debug, PartialEq, Eq)]
        enum $row {
            #[index(name = key, unique, key_type = u16, key = |payload| *payload)]
            #[index(name = group, many, key_type = u8, key = |payload| (*payload % 2) as u8)]
            First(u16),
            #[index(name = key, unique, cached = false, key_type = u16, key = |payload| *payload)]
            #[index(name = group, many, cached = false, key_type = u8, key = |payload| (*payload % 2) as u8)]
            Second(u16),
        }
    };
}

tiny_enum!(TinyItem, hasher = CollisionBuilder);
tiny_enum!(
    AppendTinyItem,
    append_only = true,
    hasher = CollisionBuilder
);

#[test]
fn custom_id_growth_and_free_list() {
    macro_rules! fill_enum {
        ($row:ident, $table:ident) => {{
            let mut table = $table::new();
            for key in 0..160 {
                let predicted: TinyId = table.next_id();
                assert_eq!(table.next_id(), predicted);
                let first: TinyId = table.insert($row::First(key)).unwrap();
                assert_eq!(first, predicted);
                let predicted: TinyId = table.next_id();
                let second: TinyId = table.insert($row::Second(key)).unwrap();
                assert_eq!(second, predicted);
                assert_eq!(first.index(), key as usize * 2);
                assert_eq!(second.index(), key as usize * 2 + 1);
                assert_ne!(first.0, first.index());
                assert_eq!(table.get(first), Some(&$row::First(key)));
                assert_eq!(table.get(second), Some(&$row::Second(key)));
                assert_eq!(table.get_first_by_key(&key).unwrap().0, first);
                assert_eq!(table.get_second_by_key(&key).unwrap().0, second);
            }
            assert_eq!(table.len(), 320);
            assert_eq!(table.iter().count(), 320);
            // Query every old key after growth has rehashed both index types.
            for key in 0..160 {
                assert_eq!(
                    table.get_first_by_key(&key).unwrap().0,
                    TinyId::new(key as usize * 2)
                );
                assert_eq!(
                    table.get_second_by_key(&key).unwrap().0,
                    TinyId::new(key as usize * 2 + 1)
                );
            }
            let predicted = table.next_id();
            let error = table.insert($row::Second(128)).unwrap_err();
            assert_eq!(table.next_id(), predicted);
            assert_eq!(error.0, $row::Second(128));
            assert_eq!(error.1, TinyId::new(257));
            assert_eq!(table.get_second_by_key(&128).unwrap().0, error.1);
            assert_eq!(table.len(), 320);
            for group in 0..2 {
                for rows in [
                    table
                        .iter_first_by_group(&group)
                        .take(257)
                        .collect::<Vec<_>>(),
                    table
                        .iter_second_by_group(&group)
                        .take(257)
                        .collect::<Vec<_>>(),
                ] {
                    assert_eq!(rows.len(), 80);
                    let mut ids: Vec<_> = rows.iter().map(|(id, _)| id.index()).collect();
                    ids.sort_unstable();
                    ids.dedup();
                    assert_eq!(ids.len(), 80); // Each ring visits each row exactly once.
                    for (id, row) in rows {
                        assert_eq!(table.get(id), Some(row));
                        let key = match row {
                            $row::First(key) | $row::Second(key) => key,
                        };
                        assert_eq!((*key % 2) as u8, group);
                    }
                }
            }
            assert!(table.iter_first_by_group(&2).next().is_none());
            table
        }};
    }
    let mut enum_table = fill_enum!(TinyItem, TinyItemTable);
    let _ = fill_enum!(AppendTinyItem, AppendTinyItemTable);
    let first = enum_table.get_first_by_key(&0).unwrap().0;
    let second = enum_table.get_second_by_key(&0).unwrap().0;
    enum_table.remove(first);
    enum_table.remove(second);
    assert_eq!(enum_table.next_id(), second);
    assert_eq!(enum_table.insert(TinyItem::First(160)).unwrap(), second);
    assert_eq!(enum_table.next_id(), first);
    assert_eq!(enum_table.insert(TinyItem::Second(160)).unwrap(), first);
    assert_eq!(enum_table.get_first_by_key(&160).unwrap().0, second);
    assert_eq!(enum_table.get_second_by_key(&160).unwrap().0, first);
    enum_table.replace(second, TinyItem::Second(161)).unwrap();
    assert!(enum_table.get_first_by_key(&160).is_none());
    assert_eq!(enum_table.get_second_by_key(&161).unwrap().0, second);
    assert_eq!(enum_table.iter_first_by_group(&0).count(), 79);
    assert_eq!(enum_table.iter_second_by_group(&0).count(), 80);
    assert_eq!(enum_table.iter_second_by_group(&1).count(), 81);

    // Remove the current representatives from both cached and noncached groups.
    let first_head = enum_table.iter_first_by_group(&0).next().unwrap().0;
    let second_head = enum_table.iter_second_by_group(&0).next().unwrap().0;
    enum_table.remove(first_head).unwrap();
    enum_table.remove(second_head).unwrap();
    assert_eq!(enum_table.iter_first_by_group(&0).count(), 78);
    assert_eq!(enum_table.iter_second_by_group(&0).count(), 79);
    let first_head = enum_table.iter_first_by_group(&0).next().unwrap().0;
    let second_head = enum_table.iter_second_by_group(&0).next().unwrap().0;
    enum_table
        .replace(first_head, TinyItem::First(162))
        .unwrap();
    enum_table
        .replace(second_head, TinyItem::Second(162))
        .unwrap();
    assert_eq!(enum_table.get_first_by_key(&162).unwrap().0, first_head);
    assert_eq!(enum_table.get_second_by_key(&162).unwrap().0, second_head);
    assert_eq!(enum_table.iter_first_by_group(&0).count(), 78);
    assert_eq!(enum_table.iter_second_by_group(&0).count(), 79);

    let mut table = TinyTable::new();
    let ids: Vec<_> = (0..320)
        .map(|value| {
            let id = table
                .insert(Tiny {
                    group: (value % 2) as u8,
                })
                .unwrap();
            assert_eq!(id.index(), value);
            assert_ne!(id.0, value);
            id
        })
        .collect();
    assert_eq!(table.len(), 320);
    assert_eq!(table.iter_by_group(&0).take(321).count(), 160);
    table.remove(ids[5]);
    table.remove(ids[8]);
    assert_eq!(table.next_id(), ids[8]);
    let a = table.insert(Tiny { group: 0 }).unwrap();
    assert_eq!(table.next_id(), ids[5]);
    let b = table.insert(Tiny { group: 0 }).unwrap();
    assert_eq!(a.index(), 8);
    assert_eq!(b.index(), 5);
    assert_eq!(table.iter_by_group(&0).take(321).count(), 161);
    assert_eq!(table.iter().count(), 320);
    table.replace(a, Tiny { group: 1 }).unwrap();
    assert_eq!(table.iter_by_group(&0).take(321).count(), 160);
    assert_eq!(table.iter_by_group(&1).take(321).count(), 160);

    let mut append = AppendTinyTable::new();
    for value in 0..320 {
        let predicted: TinyId = append.next_id();
        let id: TinyId = append
            .insert(AppendTiny {
                group: (value % 2) as u8,
            })
            .unwrap();
        assert_eq!(id, predicted);
        assert_eq!(id.index(), value);
        assert_ne!(id.0, value);
        assert!(append.contains(id));
    }
    assert_eq!(append.len(), 320);
    assert_eq!(append.iter().count(), 320);
    for group in 0..2 {
        let mut iter = append.iter_by_group(&group);
        for _ in 0..160 {
            assert_eq!(iter.next().unwrap().1.group, group);
        }
        assert!(iter.next().is_none());
        assert!(iter.next().is_none());
    }
    assert!(append.iter_by_group(&2).next().is_none());
    let id = append.insert(AppendTiny { group: 2 }).unwrap();
    assert_eq!(id.index(), 320);
    assert_eq!(append.len(), 321);
    let mut singleton = append.iter_by_group(&2);
    assert_eq!(singleton.next().unwrap().0, id);
    assert!(singleton.next().is_none());
    assert!(singleton.next().is_none());
}

#[table]
struct Plain {
    value: u32,
}

#[test]
fn integer_id_reuses_slots() {
    let mut table = PlainTable::new();
    let first: usize = table.insert(Plain { value: 1 }).ok().unwrap();
    table.remove(first);
    let next = table.insert(Plain { value: 2 }).ok().unwrap();
    assert_eq!(first, next);
    assert_eq!(table.get(first).unwrap().value, 2);
}

macro_rules! generic_row {
    ($row:ident $(, $option:meta)*) => {
        #[table($($option),*)]
        struct $row<'a, T, const N: usize>
        where
            T: PartialEq,
        {
            #[indexed(unique)]
            key: T,
            #[indexed(many)]
            label: &'a str,
            data: [u8; N],
        }
    };
}

generic_row!(Generic, hasher = StableBuilder);
generic_row!(AppendGeneric, append_only = true, hasher = CollisionBuilder);

#[test]
fn generic_rows_lifetimes_and_non_indexed_data() {
    #[derive(PartialEq, Eq, Hash)]
    struct Key(u32); // Default field indexes do not need Clone.

    macro_rules! check {
        ($row:ident, $table:ident) => {{
            let label = String::from("borrowed");
            let mut table = $table::<Key, 3>::new();
            let id = table
                .insert($row {
                    key: Key(7),
                    label: &label,
                    data: [1, 2, 3],
                })
                .ok()
                .unwrap();
            assert_eq!(table.get_by_key(&Key(7)).unwrap().0, id);
            assert_eq!(
                table.iter_by_label(&label[..]).next().unwrap().1.data,
                [1, 2, 3]
            );
            let mut singleton = table.iter_by_label("borrowed");
            assert_eq!(singleton.next().unwrap().0, id);
            assert!(singleton.next().is_none());
            assert!(singleton.next().is_none());
            drop(singleton);
            for value in 8..12 {
                table
                    .insert($row {
                        key: Key(value),
                        label: &label,
                        data: [4, 5, 6],
                    })
                    .ok()
                    .unwrap();
            }
            let matches = {
                let query = String::from("borrowed");
                table.iter_by_label(query.as_str())
            };
            let mut ids: Vec<_> = matches.take(6).map(|(id, _)| id).collect();
            ids.sort_unstable();
            assert_eq!(ids, [0, 1, 2, 3, 4]);
            let mut absent = table.iter_by_label("absent");
            assert!(absent.next().is_none());
            assert!(absent.next().is_none());
            assert_eq!(table.len(), 5);
            assert_eq!(table.iter().count(), 5);
        }};
    }
    check!(Generic, GenericTable);
    check!(AppendGeneric, AppendGenericTable);
}

#[test]
fn lookup_iterator_does_not_borrow_the_search_key() {
    let mut table = PersonTable::new();
    table.insert(person(1, "red")).unwrap();
    let matches = {
        let temporary = String::from("red");
        table.iter_by_team(temporary.as_str())
    };
    assert_eq!(matches.count(), 1);
}

// Conditional attributes must gate all generated types and impls together.
#[table]
#[cfg_attr(all(), cfg(any()), derive(Debug))]
struct Disabled {
    key: MissingType,
}

#[table]
#[cfg_attr(all(), derive(Debug))]
struct Empty {}

#[table(append_only)]
#[derive(Debug)]
struct AppendEmpty {}

#[test]
fn empty_rows_work() {
    let mut table = EmptyTable::new();
    let id = table.insert(Empty {}).unwrap();
    assert_eq!(table.iter().count(), 1);
    assert!(table.remove(id).is_some());

    let mut append = AppendEmptyTable::default();
    assert!(append.is_empty());
    assert_eq!(append.len(), 0);
    assert!(append.iter().next().is_none());
    assert!(append.get(0).is_none());
    let first = append.insert(AppendEmpty {}).unwrap();
    let second = append.insert(AppendEmpty {}).unwrap();
    assert_ne!(first, second);
    assert!(append.contains(first));
    assert!(append.get(second).is_some());
    assert_eq!(append.len(), 2);
    let mut ids: Vec<_> = append.iter().map(|(id, _)| id).collect();
    ids.sort_unstable();
    assert_eq!(ids, [first, second]);

    #[table]
    #[derive(Debug)]
    enum Singleton {
        #[index(name = value, unique, key_type = u32, key = |value| *value)]
        #[index(name = bucket, many, cached = false, key_type = u32, key = |value| *value % 2)]
        Value(u32),
    }
    let mut single = SingletonTable::new();
    let id = single.insert(Singleton::Value(2)).unwrap();
    single.replace(id, Singleton::Value(2)).unwrap();
    assert_eq!(single.get_value_by_value(&2).unwrap().0, id);
    assert_eq!(single.iter_value_by_bucket(&0).count(), 1);
    single.remove(id).unwrap();
    assert!(single.get_value_by_value(&2).is_none());
    assert!(single.iter_value_by_bucket(&0).next().is_none());

    #[table(append_only)]
    #[derive(Debug)]
    enum Unindexed {
        Value(()),
    }
    let mut unindexed = UnindexedTable::new();
    let id = unindexed.insert(Unindexed::Value(())).unwrap();
    assert!(unindexed.get(id).is_some());
    assert_eq!(unindexed.iter().count(), 1);

    #[table]
    #[derive(Debug)]
    enum Recursive {
        #[index(name = tag, unique, key_type = u8, key = |_| 0)]
        Branch(Box<Self>),
        Leaf(()),
    }
    let mut recursive = RecursiveTable::new();
    let id = recursive
        .insert(Recursive::Branch(Box::new(Recursive::Leaf(()))))
        .unwrap();
    assert_eq!(recursive.get_branch_by_tag(&0).unwrap().0, id);
    recursive.replace(id, Recursive::Leaf(())).unwrap();
    assert!(recursive.get_branch_by_tag(&0).is_none());
}

#[table]
#[derive(Debug)]
struct Signed {
    #[indexed(unique)]
    r#type: u16,
}

#[test]
fn growth_and_raw_field_names() {
    let mut table = SignedTable::new();
    for key in 0..300 {
        let id: usize = table.insert(Signed { r#type: key }).unwrap();
        assert_eq!(id, key as usize);
        assert_eq!(table.get_by_type(&key).unwrap().0, id);
    }
    assert_eq!(table.len(), 300);
    assert_eq!(table.get_by_type(&128).unwrap().0, 128);
    assert_eq!(table.get_by_type(&256).unwrap().0, 256);
    let first = table.get_by_type(&0).unwrap().0;
    table.remove(first);
    let reused = table.insert(Signed { r#type: 300 }).unwrap();
    assert_eq!(reused, first);
    assert_eq!(table.get_by_type(&300).unwrap().0, reused);
}

#[test]
fn randomized_operations_match_a_scan_model() {
    let mut table = PersonTable::new();
    let mut model = std::collections::HashMap::<usize, Person>::new();
    let mut history = Vec::new();
    let mut seed = 42u64;
    for _ in 0..3000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let number = ((seed >> 32) % 40) as u32;
        let team = ["red", "blue", "green"][(seed >> 40) as usize % 3];
        let row = person(number, team);
        let chosen = (!history.is_empty()).then(|| history[(seed >> 16) as usize % history.len()]);
        match (seed % 3, chosen) {
            (0, _) | (_, None) => {
                let conflict = model
                    .iter()
                    .find_map(|(&id, old)| (old.number == number).then_some(id));
                match table.insert(row) {
                    Ok(id) => {
                        assert!(conflict.is_none());
                        model.insert(id, person(number, team));
                        history.push(id);
                    }
                    Err((rejected, id)) => {
                        assert_eq!(rejected, person(number, team));
                        assert_eq!(Some(id), conflict);
                    }
                }
            }
            (1, Some(id)) => {
                assert_eq!(table.remove(id), model.remove(&id));
            }
            (_, Some(id)) => {
                let conflict = model
                    .iter()
                    .any(|(&other, old)| other != id && old.number == number);
                match table.replace(id, row) {
                    Ok(old) => {
                        assert!(!conflict);
                        assert_eq!(model.insert(id, person(number, team)), Some(old));
                    }
                    Err(error) => assert!(conflict || !model.contains_key(&id), "{error}"),
                }
            }
        }
        assert_eq!(table.len(), model.len());
        assert_eq!(table.iter().count(), model.len());
        for (id, row) in &model {
            assert_eq!(table.get(*id), Some(row));
            assert_eq!(table.get_by_number(&row.number).unwrap().0, *id);
            assert_eq!(table.get_by_email(row.email.as_str()).unwrap().0, *id);
        }
        for team in ["red", "blue", "green"] {
            let mut expected: Vec<_> = model
                .values()
                .filter(|row| row.team == team)
                .map(|row| row.number)
                .collect();
            expected.sort_unstable();
            assert_eq!(members(&table, team), expected);
        }
        for active in [true, false] {
            let mut actual: Vec<_> = table.iter_by_active(&active).map(|(id, _)| id).collect();
            let mut expected: Vec<_> = model
                .iter()
                .filter(|(_, row)| row.active == active)
                .map(|(&id, _)| id)
                .collect();
            actual.sort_unstable();
            expected.sort_unstable();
            assert_eq!(actual, expected);
        }
    }
}
