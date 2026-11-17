use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelRow {
    unique: Option<String>,
    group: Option<String>,
}

microtable::table! {
    #[table(hasher = ConstantState)]
    #[derive(Clone, Debug)]
    struct ModelTable([ModelRow]);

    #[index(unique, optional)]
    fn unique(row: &ModelRow) -> Option<&str> {
        row.unique.as_deref()
    }

    #[index(unique, optional, cached)]
    fn cached_unique(row: &ModelRow) -> Option<String> {
        row.unique.clone()
    }

    #[index(many, optional)]
    fn group(row: &ModelRow) -> Option<&str> {
        row.group.as_deref()
    }

    #[index(many, optional, cached)]
    fn cached_group(row: &ModelRow) -> Option<String> {
        row.group.clone()
    }
}

#[derive(Clone, Default)]
struct ConstantState;

struct ConstantHasher;

impl std::hash::Hasher for ConstantHasher {
    fn finish(&self) -> u64 {
        0
    }

    fn write(&mut self, _: &[u8]) {}
}

impl std::hash::BuildHasher for ConstantState {
    type Hasher = ConstantHasher;

    fn build_hasher(&self) -> Self::Hasher {
        ConstantHasher
    }
}

fn random(state: &mut u64) -> usize {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
    (*state >> 32) as usize
}

fn row(state: &mut u64) -> ModelRow {
    let unique = random(state) % 41;
    let group = random(state) % 9;
    ModelRow {
        unique: (unique != 0).then(|| format!("key{unique}")),
        group: (group != 0).then(|| format!("group{group}")),
    }
}

fn blocking(
    rows: &BTreeMap<usize, ModelRow>,
    candidate: &ModelRow,
    exclude: Option<usize>,
) -> Option<usize> {
    candidate.unique.as_ref().and_then(|key| {
        rows.iter().find_map(|(&id, stored)| {
            (Some(id) != exclude && stored.unique.as_ref() == Some(key)).then_some(id)
        })
    })
}

fn verify(table: &ModelTable, expected: &BTreeMap<usize, ModelRow>) {
    assert_eq!(table.len(), expected.len());
    assert_eq!(table.is_empty(), expected.is_empty());
    let actual: BTreeMap<_, _> = table.iter().map(|(id, row)| (id, row.clone())).collect();
    assert_eq!(&actual, expected);
    for key in 0..42 {
        let key = format!("key{key}");
        let expected_id = expected
            .iter()
            .find_map(|(&id, row)| (row.unique.as_deref() == Some(&key)).then_some(id));
        assert_eq!(
            table.get_by_unique(key.as_str()).map(|(id, _)| id),
            expected_id
        );
        assert_eq!(
            table.get_by_cached_unique(key.as_str()).map(|(id, _)| id),
            expected_id
        );
    }
    for group in 0..10 {
        let key = format!("group{group}");
        let expected_ids: BTreeSet<_> = expected
            .iter()
            .filter_map(|(&id, row)| (row.group.as_deref() == Some(&key)).then_some(id))
            .collect();
        let borrowed: Vec<_> = table
            .iter_by_group(key.as_str())
            .map(|(id, _)| id)
            .collect();
        let cached: Vec<_> = table
            .iter_by_cached_group(key.as_str())
            .map(|(id, _)| id)
            .collect();
        assert_eq!(borrowed.len(), expected_ids.len());
        assert_eq!(cached.len(), expected_ids.len());
        assert_eq!(borrowed.into_iter().collect::<BTreeSet<_>>(), expected_ids);
        assert_eq!(cached.into_iter().collect::<BTreeSet<_>>(), expected_ids);
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct BorrowedKey<'a>(&'a str);

impl Drop for BorrowedKey<'_> {
    fn drop(&mut self) {
        // The key remains a real row borrow until its destructor runs.
        std::hint::black_box(self.0);
    }
}

microtable::table! {
    #[derive(Clone)]
    struct BorrowedKeys([String]);

    #[index(unique)]
    fn wrapped(row: &String) -> BorrowedKey<'_> {
        BorrowedKey(row.as_str())
    }

    #[index(many)]
    fn words(row: &String) -> Vec<&str> {
        row.split_whitespace().collect()
    }
}

#[test]
fn keys_can_contain_borrows_inside_custom_types_and_containers() {
    let mut table = BorrowedKeys::new();
    let id = table.insert("hello world".into()).unwrap();
    assert_eq!(table.insert("hello world".into()).unwrap_err().1, id);
    let second = table.insert("goodbye world".into()).unwrap();
    assert!(table.replace(second, "hello world".into()).is_err());
    assert_eq!(
        table.replace(second, "other words".into()).unwrap(),
        "goodbye world"
    );
    assert_eq!(
        table.get_by_wrapped(&BorrowedKey("hello world")).unwrap().0,
        id
    );
    let matches = {
        let hello = String::from("hello");
        let world = String::from("world");
        table.iter_by_words(&vec![hello.as_str(), world.as_str()])
    };
    assert_eq!(matches.map(|(id, _)| id).collect::<Vec<_>>(), vec![id]);
    assert_eq!(
        table
            .clone()
            .get_by_wrapped(&BorrowedKey("hello world"))
            .unwrap()
            .0,
        id
    );
}

mod attributes_and_names {
    #![allow(dead_code)]

    struct None;
    struct Some;
    struct Ok;
    struct Err;

    microtable::table! {
        #[repr(C)]
        #[cfg_attr(all(), repr(align(64)))]
        #[derive(Clone)]
        struct AlignedRows([u32]);

        #[index(unique)]
        fn key(row: &u32) -> u32 { *row }

        #[index(many)]
        fn parity(row: &u32) -> u32 { row % 2 }
    }

    #[test]
    fn table_attributes_and_shadowed_prelude_variants_work() {
        assert!(std::mem::align_of::<AlignedRows>() >= 64);
        let mut table = AlignedRows::new();
        let id = table.insert(4).unwrap();
        assert_eq!(table.get_by_key(&4).unwrap().0, id);
        assert_eq!(table.iter_by_parity(&0).count(), 1);
        assert_eq!(table.replace(id, 6).unwrap(), 4);
        let mut cloned = table.clone();
        assert_eq!(cloned.remove(id), std::option::Option::Some(6));
    }
}

#[test]
fn optional_indexes_match_a_model_through_colliding_mutations() {
    let mut table = ModelTable::new();
    let mut expected = BTreeMap::new();
    let mut state = 0x1234_5678_9abc_def0;

    for step in 0..2400 {
        match random(&mut state) % 3 {
            0 => {
                let candidate = row(&mut state);
                let conflict = blocking(&expected, &candidate, None);
                let next = conflict.is_none().then(|| table.next_id());
                match table.insert(candidate.clone()) {
                    Ok(id) => {
                        assert_eq!(conflict, None);
                        assert_eq!(Some(id), next);
                        assert!(expected.insert(id, candidate).is_none());
                    }
                    Err((rejected, id)) => {
                        assert_eq!(Some(id), conflict);
                        assert_eq!(rejected, candidate);
                    }
                }
            }
            1 => {
                let id = random(&mut state) % 64;
                assert_eq!(table.remove(id), expected.remove(&id));
            }
            _ => {
                let id = random(&mut state) % 64;
                let candidate = row(&mut state);
                let conflict = blocking(&expected, &candidate, Some(id));
                match table.replace(id, candidate.clone()) {
                    Ok(previous) => {
                        assert_eq!(conflict, None);
                        assert_eq!(Some(previous), expected.insert(id, candidate));
                    }
                    Err(error) => {
                        assert_eq!(error.row, candidate);
                        if expected.contains_key(&id) {
                            assert!(conflict.is_some());
                            assert_eq!(
                                error.kind,
                                microtable::ReplaceErrorKind::Unique { field: "unique" }
                            );
                        } else {
                            assert_eq!(error.kind, microtable::ReplaceErrorKind::NotFound);
                        }
                    }
                }
            }
        }
        verify(&table, &expected);
        if step % 101 == 0 {
            verify(&table.clone(), &expected);
        }
    }
}
