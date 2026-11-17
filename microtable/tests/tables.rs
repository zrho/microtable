use microtable::ReplaceErrorKind;
use std::collections::HashMap;

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
pub struct Person {
    email: String,
    number: u32,
    team: String,
    active: bool,
    payload: Vec<u8>,
}

microtable::table! {
    #[table(hasher = CollisionBuilder)]
    #[derive(Debug, Clone)]
    pub struct People([Person]);
    #[id]
    pub struct PersonId(u32);
    #[index(unique, cached)]
    fn email(row: &Person) -> String { row.email.clone() }
    #[index(unique)]
    fn number(row: &Person) -> u32 { row.number }
    #[index(many)]
    fn team(row: &Person) -> &str { &row.team }
    #[index(many, cached)]
    fn active(row: &Person) -> bool { row.active }
}

fn person(number: u32, team: &str) -> Person {
    Person {
        email: format!("{number}@example.com"),
        number,
        team: team.into(),
        active: number.is_multiple_of(2),
        payload: vec![number as u8],
    }
}
fn members(table: &People, team: &str) -> Vec<u32> {
    let mut rows: Vec<_> = table
        .iter_by_team(team)
        .map(|(_, row)| row.number)
        .collect();
    rows.sort_unstable();
    rows
}

#[test]
fn insertion_checks_unique_indexes_in_order_without_consuming_ids() {
    let mut table = People::default();
    assert!(table.is_empty());
    let predicted = table.next_id();
    assert_eq!(predicted, table.next_id());
    assert!(!table.contains(predicted));
    let a = table.insert(person(1, "red")).unwrap();
    let b = table.insert(person(2, "red")).unwrap();
    assert_eq!(a, predicted);
    assert_eq!(table.get_by_email("1@example.com").unwrap().0, a);
    assert_eq!(table.get_by_number(&2).unwrap().0, b);
    let next = table.next_id();
    let mut rejected = person(2, "blue");
    rejected.email = "1@example.com".into();
    let error = table.insert(rejected.clone()).unwrap_err();
    assert_eq!(error, (rejected, a));
    let mut rejected = person(2, "blue");
    rejected.email = "unused".into();
    assert_eq!(table.insert(rejected.clone()).unwrap_err(), (rejected, b));
    assert_eq!(table.next_id(), next);
    assert!(table.get_by_email("unused").is_none());
    assert_eq!(members(&table, "red"), [1, 2]);
    assert!(members(&table, "blue").is_empty());
    assert_eq!(table.len(), 2);
}

#[test]
fn replacement_is_atomic_and_accepts_its_own_unique_keys() {
    let mut table = People::new();
    let a = table.insert(person(1, "red")).unwrap();
    let b = table.insert(person(2, "red")).unwrap();
    for (email, number, field) in [("1@example.com", 3, "email"), ("unused", 1, "number")] {
        let mut row = person(number, "blue");
        row.email = email.into();
        let error = table.replace(b, row.clone()).unwrap_err();
        assert_eq!(error.kind, ReplaceErrorKind::Unique { field });
        assert_eq!(error.row, row);
        assert_eq!(table.get(b), Some(&person(2, "red")));
        assert_eq!(members(&table, "red"), [1, 2]);
        assert!(members(&table, "blue").is_empty());
        assert!(table.get_by_number(&3).is_none());
        assert!(table.get_by_email("unused").is_none());
    }
    assert_eq!(
        table.replace(a, person(1, "blue")).unwrap(),
        person(1, "red")
    );
    table.replace(a, person(1, "blue")).unwrap();
    assert_eq!(members(&table, "red"), [2]);
    assert_eq!(members(&table, "blue"), [1]);
    assert_eq!(
        table.replace(a, person(4, "red")).unwrap(),
        person(1, "blue")
    );
    assert!(table.get_by_email("1@example.com").is_none());
    assert!(table.get_by_number(&1).is_none());
    assert_eq!(table.get_by_number(&4).unwrap().0, a);
    assert_eq!(table.iter_by_active(&true).count(), 2);
    table.remove(a).unwrap();
    let error = table.replace(a, person(5, "green")).unwrap_err();
    assert_eq!(error.kind, ReplaceErrorKind::NotFound);
    assert_eq!(error.row, person(5, "green"));
    assert!(table.get_by_number(&5).is_none());
}

#[test]
fn free_list_is_lifo_and_does_not_move_live_rows() {
    let mut table = People::new();
    let a = table.insert(person(1, "red")).unwrap();
    let b = table.insert(person(2, "red")).unwrap();
    let c = table.insert(person(3, "red")).unwrap();
    assert_eq!(table.remove(a), Some(person(1, "red")));
    assert_eq!(table.remove(a), None);
    table.remove(c).unwrap();
    assert_eq!(table.next_id(), c);
    assert_eq!(table.insert(person(4, "blue")).unwrap(), c);
    assert_eq!(table.next_id(), a);
    assert_eq!(table.insert(person(5, "blue")).unwrap(), a);
    assert_eq!(table.get(b), Some(&person(2, "red")));
    assert_eq!(table.get(a), Some(&person(5, "blue")));
    assert_eq!(table.len(), 3);
}

#[test]
fn many_lists_remove_middle_tail_head_and_singleton() {
    let mut table = People::new();
    let ids: Vec<_> = (1..=4)
        .map(|n| table.insert(person(n, "red")).unwrap())
        .collect();
    table.remove(ids[1]);
    assert_eq!(members(&table, "red"), [1, 3, 4]);
    table.remove(ids[0]);
    assert_eq!(members(&table, "red"), [3, 4]);
    let head = table.iter_by_team("red").next().unwrap().0;
    table.remove(head);
    assert_eq!(table.iter_by_team("red").count(), 1);
    let last = table.iter_by_team("red").next().unwrap().0;
    table.remove(last);
    assert!(table.is_empty());
    assert_eq!(table.iter_by_active(&true).count(), 0);
    assert_eq!(table.iter_by_active(&false).count(), 0);
    table.insert(person(5, "red")).unwrap();
    assert_eq!(members(&table, "red"), [5]);
}

#[test]
fn collision_rehash_and_group_representative_replacement() {
    let mut table = People::new();
    for n in 0..320 {
        table
            .insert(person(n, if n % 2 == 0 { "red" } else { "blue" }))
            .unwrap();
    }
    for n in 0..320 {
        let id = table.get_by_number(&n).unwrap().0;
        assert_eq!(id.index(), n as usize);
        assert_eq!(
            table
                .get_by_email(format!("{n}@example.com").as_str())
                .unwrap()
                .0,
            id
        );
    }
    let head = table.iter_by_team("red").next().unwrap().0;
    let number = table.get(head).unwrap().number;
    table.replace(head, person(number, "green")).unwrap();
    assert_eq!(members(&table, "red").len(), 159);
    assert_eq!(members(&table, "green"), [number]);
    let promoted = table.iter_by_team("red").next().unwrap().0;
    table.remove(promoted).unwrap();
    assert_eq!(members(&table, "red").len(), 158);
    for n in 320..640 {
        table.insert(person(n, "red")).unwrap();
    }
    assert_eq!(members(&table, "red").len(), 478);
    assert_eq!(members(&table, "blue").len(), 160);
}

#[test]
fn query_iterator_does_not_retain_query_lifetime_and_is_fused() {
    let mut table = People::new();
    for n in 0..5 {
        table.insert(person(n, "red")).unwrap();
    }
    let matches = {
        let query = String::from("red");
        table.iter_by_team(query.as_str())
    };
    assert_eq!(matches.take(6).count(), 5);
    let mut absent = table.iter_by_team("absent");
    assert!(absent.next().is_none());
    assert!(absent.next().is_none());
    let mut cached = table.iter_by_active(&false);
    assert!(cached.next().is_some());
    assert!(cached.next().is_some());
    assert!(cached.next().is_none());
    assert!(cached.next().is_none());
}

#[test]
fn clone_and_debug_preserve_indexes_and_independent_storage() {
    let mut original = People::new();
    let a = original.insert(person(1, "red")).unwrap();
    let b = original.insert(person(2, "red")).unwrap();
    original.remove(a);
    let mut cloned = original.clone();
    assert_eq!(original.next_id(), cloned.next_id());
    assert_eq!(cloned.get_by_number(&2).unwrap().0, b);
    assert_eq!(members(&cloned, "red"), [2]);
    assert!(!format!("{cloned:?}").is_empty());
    cloned.replace(b, person(3, "blue")).unwrap();
    assert_eq!(original.get(b), Some(&person(2, "red")));
    assert_eq!(cloned.insert(person(4, "blue")).unwrap(), a);
    assert!(!original.contains(a));
    assert_eq!(original.insert(person(5, "green")).unwrap(), a);
    assert_eq!(members(&cloned, "blue"), [3, 4]);
    assert_eq!(members(&original, "green"), [5]);
}

#[test]
fn randomized_operations_match_scan_model() {
    let mut table = People::new();
    let mut model = HashMap::<PersonId, Person>::new();
    let mut history = Vec::new();
    let mut seed = 42u64;
    for _ in 0..1500 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let number = ((seed >> 32) % 40) as u32;
        let team = ["red", "blue", "green"][(seed >> 40) as usize % 3];
        let chosen = (!history.is_empty()).then(|| history[(seed >> 16) as usize % history.len()]);
        let conflict = model
            .iter()
            .find_map(|(&id, row)| (row.number == number).then_some(id));
        match (seed % 3, chosen) {
            (0, _) | (_, None) => match table.insert(person(number, team)) {
                Ok(id) => {
                    assert!(conflict.is_none());
                    model.insert(id, person(number, team));
                    history.push(id);
                }
                Err((row, id)) => {
                    assert_eq!(row, person(number, team));
                    assert_eq!(Some(id), conflict);
                }
            },
            (1, Some(id)) => assert_eq!(table.remove(id), model.remove(&id)),
            (_, Some(id)) => match table.replace(id, person(number, team)) {
                Ok(old) => {
                    assert!(conflict.is_none() || conflict == Some(id));
                    assert_eq!(model.insert(id, person(number, team)), Some(old));
                }
                Err(error) => {
                    assert!(conflict.is_some_and(|other| other != id) || !model.contains_key(&id));
                    assert_eq!(error.row, person(number, team));
                }
            },
        }
        assert_eq!(table.len(), model.len());
        assert_eq!(table.iter().count(), model.len());
        for (&id, row) in &model {
            assert_eq!(table.get(id), Some(row));
            assert_eq!(table.get_by_email(row.email.as_str()).unwrap().0, id);
            assert_eq!(table.get_by_number(&row.number).unwrap().0, id);
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
        for active in [false, true] {
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
