use minitable::{InsertErrorKind, ReplaceErrorKind, table};

#[table]
#[derive(Debug, PartialEq, Eq)]
pub struct Person {
    #[indexed(unique)]
    email: String,
    #[indexed(unique)]
    number: u32,
    #[indexed(many)]
    team: String,
    #[indexed(many)]
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
    let id = table.insert(person(1, "red")).unwrap();
    assert_eq!(table.get_by_email("1@example.com").unwrap().0, id);
    assert_eq!(table.get_by_number(&1).unwrap().0, id);
    let mut rejected = person(1, "blue");
    rejected.email = "other@example.com".into();
    let error = table.insert(rejected).unwrap_err();
    assert_eq!(error.kind, InsertErrorKind::Unique { field: "number" });
    assert_eq!(error.row.email, "other@example.com");
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

#[table(index_type = u8)]
#[derive(Debug)]
struct CountedRow {
    #[indexed(unique)]
    first: CountedKey,
    #[indexed(unique)]
    second: CountedKey,
    #[indexed(many)]
    group: u8,
}

impl CountedRow {
    fn new(first: u32, second: u32, group: u8) -> Self {
        Self {
            first: CountedKey::new(first),
            second: CountedKey::new(second),
            group,
        }
    }
}

#[test]
fn unique_entries_hash_each_new_key_once() {
    let mut table = CountedRowTable::new();
    // Use nonempty maps so a separate validation lookup would hash the key.
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
    assert_eq!(first_clones.get(), 1);
    assert_eq!(second_clones.get(), 1);

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
    assert_eq!(new_first_clones.get(), 1);
    assert_eq!(new_second_clones.get(), 0);
    // Only the changed old key needs a removal lookup.
    assert_eq!(first_hashes.get(), 2);
    assert_eq!(second_hashes.get(), 1);
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
        let error = table.insert(CountedRow::new(first, second, 1)).unwrap_err();
        assert_eq!(error.kind, InsertErrorKind::Unique { field });
        assert_eq!(error.row.first.clones.get(), 0);
        assert_eq!(error.row.second.clones.get(), 0);
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
    table.remove(b);
    let error = table.replace(b, CountedRow::new(2, 2, 1)).unwrap_err();
    assert_eq!(error.kind, ReplaceErrorKind::NotFound);
    assert_eq!(error.row.first.clones.get(), 0);
    assert_eq!(error.row.second.clones.get(), 0);
}

#[test]
fn exhausted_capacity_does_not_clone_keys() {
    let mut table = CountedRowTable::new();
    for value in 0..256 {
        table.insert(CountedRow::new(value, value, 0)).unwrap();
    }
    let error = table.insert(CountedRow::new(256, 256, 1)).unwrap_err();
    assert_eq!(error.kind, InsertErrorKind::CapacityExhausted);
    assert_eq!(error.row.first.clones.get(), 0);
    assert_eq!(error.row.second.clones.get(), 0);
    assert!(table.get_by_first(&256).is_none());
    assert!(table.get_by_second(&256).is_none());
    assert_eq!(table.iter_by_group(&1).count(), 0);
    let first = table.get_by_first(&0).unwrap().0;
    table.remove(first);
    let reused = table.insert(error.row).unwrap();
    assert_eq!(reused, first);
    assert_eq!(table.get_by_first(&256).unwrap().0, reused);
}

#[table]
#[index(name = pair, unique, fields(first, second))]
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
    for (hashes, clones) in counters {
        assert_eq!(hashes.get(), 1);
        assert_eq!(clones.get(), 1);
    }
    // A composite conflict and a later computed-key conflict both defer all cloning.
    for (row, index) in [
        (NamedCountedRow::new(1, 2, 11, 6), "pair"),
        (NamedCountedRow::new(3, 4, 10, 6), "computed"),
    ] {
        let error = table.insert(row).unwrap_err();
        assert_eq!(error.kind, InsertErrorKind::Unique { field: index });
        for key in [
            &error.row.first,
            &error.row.second,
            &error.row.derived,
            &error.row.group,
        ] {
            assert_eq!(key.clones.get(), 0);
        }
    }
    let row = NamedCountedRow::new(1, 2, 11, 6);
    let first_clones = row.first.clones.clone();
    let second_clones = row.second.clones.clone();
    table.replace(id, row).unwrap();
    assert_eq!(first_clones.get(), 0);
    assert_eq!(second_clones.get(), 0);
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
struct TinyId(u8);

impl TinyId {
    fn new(index: u8) -> Self {
        Self(index)
    }

    fn index(self) -> u8 {
        self.0
    }
}

#[table(
    table = TinyTable,
    id = TinyId,
    index_type = u8,
    id_from_index = TinyId::new,
    id_to_index = |id| id.index(),
)]
#[derive(Debug)]
struct Tiny {
    #[indexed(many)]
    group: u8,
}

#[test]
fn narrow_id_capacity_and_free_list() {
    let mut table = TinyTable::new();
    let ids: Vec<_> = (0..=u8::MAX)
        .map(|group| table.insert(Tiny { group }).unwrap())
        .collect();
    assert_eq!(table.len(), 256);
    let error = table.insert(Tiny { group: 0 }).unwrap_err();
    assert_eq!(error.kind, InsertErrorKind::CapacityExhausted);
    assert_eq!(error.row.group, 0);
    assert_eq!(table.iter_by_group(&0).count(), 1);
    table.remove(ids[5]);
    table.remove(ids[8]);
    let a = table.insert(Tiny { group: 0 }).unwrap();
    let b = table.insert(Tiny { group: 0 }).unwrap();
    assert_eq!(a.index(), 8);
    assert_eq!(b.index(), 5);
    assert_eq!(table.iter_by_group(&0).count(), 3);
    assert_eq!(table.iter().count(), 256);
}

#[table(index_type = u16)]
struct Plain {
    value: u32,
}

#[test]
fn integer_id_reuses_slots() {
    let mut table = PlainTable::new();
    let first: u16 = table.insert(Plain { value: 1 }).ok().unwrap();
    table.remove(first);
    let next = table.insert(Plain { value: 2 }).ok().unwrap();
    assert_eq!(first, next);
    assert_eq!(table.get(first).unwrap().value, 2);
}

#[table]
struct Generic<'a, T, const N: usize>
where
    T: PartialEq,
{
    #[indexed(unique)]
    key: T,
    #[indexed(many)]
    label: &'a str,
    data: [u8; N],
}

#[test]
fn generic_rows_lifetimes_and_non_indexed_data() {
    let label = String::from("borrowed");
    let mut table = GenericTable::<String, 3>::new();
    let id = table
        .insert(Generic {
            key: "key".into(),
            label: &label,
            data: [1, 2, 3],
        })
        .ok()
        .unwrap();
    assert_eq!(table.get_by_key("key").unwrap().0, id);
    assert_eq!(
        table.iter_by_label(&label[..]).next().unwrap().1.data,
        [1, 2, 3]
    );
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

#[test]
fn empty_rows_work() {
    let mut table = EmptyTable::new();
    let id = table.insert(Empty {}).unwrap();
    assert_eq!(table.iter().count(), 1);
    assert!(table.remove(id).is_some());
}

#[table(index_type = i8)]
#[derive(Debug)]
struct Signed {
    #[indexed(unique)]
    r#type: u8,
}

#[test]
fn signed_backing_and_raw_field_names() {
    let mut table = SignedTable::new();
    for key in 0..128 {
        let id = table.insert(Signed { r#type: key }).unwrap();
        assert_eq!(id, key as i8);
        assert_eq!(table.get_by_type(&key).unwrap().0, id);
    }
    assert_eq!(
        table.insert(Signed { r#type: 128 }).unwrap_err().kind,
        InsertErrorKind::CapacityExhausted
    );
    assert!(table.get_by_type(&128).is_none());
    let first = table.get_by_type(&0).unwrap().0;
    table.remove(first);
    let reused = table.insert(Signed { r#type: 128 }).unwrap();
    assert_eq!(reused, first);
    assert_eq!(table.get_by_type(&128).unwrap().0, reused);
}

#[test]
fn randomized_operations_match_a_scan_model() {
    let mut table = PersonTable::new();
    let mut model = std::collections::HashMap::new();
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
                let conflict = model.values().any(|old: &Person| old.number == number);
                match table.insert(row) {
                    Ok(id) => {
                        assert!(!conflict);
                        model.insert(id, person(number, team));
                        history.push(id);
                    }
                    Err(_) => assert!(conflict),
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
