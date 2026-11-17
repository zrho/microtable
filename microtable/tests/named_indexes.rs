use std::hash::Hash;

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
struct Member {
    tenant: u32,
    name: String,
    category: String,
}
microtable::table! {
    #[table(hasher = CollisionBuilder)]
    struct Members([Member]);
    #[index(unique)]
    fn pair(row: &Member) -> (u32, &str) { (row.tenant, &row.name) }
    #[index(unique, cached)]
    fn normalized(row: &Member) -> (u32, String) { (row.tenant, row.name.to_lowercase()) }
    #[index(many)]
    fn nested(row: &Member) -> ((u32, &str), (&str, u8)) {
        ((row.tenant, &row.category), (&row.name, 7))
    }
    #[index(many, cached)]
    fn cached_pair(row: &Member) -> (u32, String) { (row.tenant, row.category.clone()) }
}
fn member(tenant: u32, name: &str, category: &str) -> Member {
    Member {
        tenant,
        name: name.into(),
        category: category.into(),
    }
}

#[test]
fn composite_queries_take_one_tuple_reference_and_compare_borrowed_strings() {
    let mut table = Members::new();
    let a = table.insert(member(1, "Ada", "red")).unwrap();
    let b = table.insert(member(2, "Ada", "red")).unwrap();
    assert_eq!(table.get_by_pair(&(1, "Ada")).unwrap().0, a);
    assert_eq!(table.get_by_pair(&(2, "Ada")).unwrap().0, b);
    assert!(table.get_by_pair(&(3, "Ada")).is_none());
    assert_eq!(table.get_by_normalized(&(1, "ada")).unwrap().0, a);
    assert_eq!(table.iter_by_cached_pair(&(1, "red")).next().unwrap().0, a);
    assert_eq!(table.insert(member(1, "aDA", "blue")).unwrap_err().1, a);
    assert_eq!(table.len(), 2);
    assert_eq!(table.iter_by_cached_pair(&(1, "blue")).count(), 0);
}

#[test]
fn nested_borrowed_tuple_queries_do_not_retain_temporary_query_data() {
    let mut table = Members::new();
    let a = table.insert(member(1, "Ada", "red")).unwrap();
    let matches = {
        let category = String::from("red");
        let name = String::from("Ada");
        let query = ((1, category.as_str()), (name.as_str(), 7));
        table.iter_by_nested(&query)
    };
    assert_eq!(matches.map(|(id, _)| id).collect::<Vec<_>>(), [a]);
    let cached = {
        let category = String::from("red");
        table.iter_by_cached_pair(&(1, category.as_str()))
    };
    assert_eq!(cached.count(), 1);
    table.replace(a, member(3, "Grace", "blue")).unwrap();
    assert!(table.get_by_pair(&(1, "Ada")).is_none());
    assert_eq!(table.iter_by_nested(&((1, "red"), ("Ada", 7))).count(), 0);
    assert_eq!(
        table
            .iter_by_nested(&((3, "blue"), ("Grace", 7)))
            .next()
            .unwrap()
            .0,
        a
    );
    table.remove(a).unwrap();
    assert!(table.get_by_normalized(&(3, "grace")).is_none());
    assert_eq!(table.iter_by_cached_pair(&(3, "blue")).count(), 0);
}

#[derive(Debug)]
struct Record<'a, T, const N: usize> {
    key: T,
    label: &'a str,
    data: [u8; N],
}
microtable::table! {
    struct Records<'a, T, const N: usize>([Record<'a, T, N>]) where T: Eq + Hash;
    #[index(unique)]
    fn key(row: &Record<'a, T, N>) -> &T { &row.key }
    #[index(many)]
    fn label(row: &Record<'a, T, N>) -> &'a str { row.label }
    #[index(many, cached)]
    fn cached_label(row: &Record<'a, T, N>) -> &'a str { row.label }
    #[index(many)]
    fn nested(row: &Record<'a, T, N>) -> ((&T, &'a str), usize) { ((&row.key, row.label), N) }
}
microtable::table! {
    #[table(append_only, hasher = CollisionBuilder)]
    struct AppendRecords<'a, T, const N: usize>([Record<'a, T, N>]) where T: Eq + Hash;
    #[index(unique)]
    fn key(row: &Record<'a, T, N>) -> &T { &row.key }
    #[index(many)]
    fn label(row: &Record<'a, T, N>) -> &'a str { row.label }
    #[index(many, cached)]
    fn cached_label(row: &Record<'a, T, N>) -> &'a str { row.label }
}

#[test]
fn generics_const_parameters_nonclone_keys_and_nonstatic_rows() {
    #[derive(Debug, PartialEq, Eq, Hash)]
    struct Key(u32);
    let label = String::from("borrowed");
    let mut table = Records::<Key, 3>::new();
    let id = table
        .insert(Record {
            key: Key(7),
            label: &label,
            data: [1, 2, 3],
        })
        .unwrap();
    assert_eq!(table.get_by_key(&Key(7)).unwrap().0, id);
    assert_eq!(
        table
            .iter_by_nested(&((&Key(7), "borrowed"), 3))
            .next()
            .unwrap()
            .0,
        id
    );
    assert_eq!(table.get(id).unwrap().data, [1, 2, 3]);
    let matches = {
        let query = String::from("borrowed");
        table.iter_by_label(query.as_str())
    };
    assert_eq!(matches.count(), 1);
    let cached = {
        let query = String::from("borrowed");
        table.iter_by_cached_label(query.as_str())
    };
    assert_eq!(cached.count(), 1);
    table
        .replace(
            id,
            Record {
                key: Key(8),
                label: &label,
                data: [4, 5, 6],
            },
        )
        .unwrap();
    assert!(table.get_by_key(&Key(7)).is_none());
    assert_eq!(table.get_by_key(&Key(8)).unwrap().0, id);
    table.remove(id).unwrap();
    assert!(table.iter_by_cached_label("borrowed").next().is_none());
}

#[test]
fn append_generics_and_cached_keys_can_borrow_external_nonstatic_data() {
    #[derive(Debug, PartialEq, Eq, Hash)]
    struct Key(u32);
    let label = String::from("borrowed");
    let mut table = AppendRecords::<Key, 2>::new();
    for n in 0..100 {
        table
            .insert(Record {
                key: Key(n),
                label: &label,
                data: [1, 2],
            })
            .unwrap();
    }
    assert_eq!(table.get_by_key(&Key(99)).unwrap().1.data, [1, 2]);
    let matches = {
        let query = String::from("borrowed");
        table.iter_by_label(query.as_str())
    };
    assert_eq!(matches.take(101).count(), 100);
    let matches = {
        let query = String::from("borrowed");
        table.iter_by_cached_label(query.as_str())
    };
    assert_eq!(matches.take(101).count(), 100);
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Idle,
    Pair(u32, String),
    Named { key: u32, group: String },
    Recursive(Box<Event>),
}
impl Event {
    fn parts(&self) -> Option<(u32, &str)> {
        match self {
            Self::Pair(key, group) | Self::Named { key, group } => Some((*key, group)),
            Self::Recursive(inner) => inner.parts(),
            Self::Idle => None,
        }
    }
}
microtable::table! {
    #[table(hasher = CollisionBuilder)]
    #[derive(Clone, Debug)]
    struct Events([Event]);
    #[index(unique, optional)]
    fn key(row: &Event) -> Option<u32> { row.parts().map(|(key, _)| key) }
    #[index(many, optional)]
    fn group(row: &Event) -> Option<&str> { row.parts().map(|(_, group)| group) }
    #[index(many, optional, cached)]
    fn cached_group(row: &Event) -> Option<String> { row.parts().map(|(_, group)| group.to_owned()) }
}

#[test]
fn arbitrary_enum_shapes_share_whole_row_indexes_and_unique_domain() {
    let mut table = Events::new();
    let idle = table.insert(Event::Idle).unwrap();
    let a = table.insert(Event::Pair(1, "red".into())).unwrap();
    let b = table
        .insert(Event::Named {
            key: 2,
            group: "red".into(),
        })
        .unwrap();
    let c = table
        .insert(Event::Recursive(Box::new(Event::Pair(3, "red".into()))))
        .unwrap();
    assert_eq!(
        table
            .insert(Event::Named {
                key: 1,
                group: "blue".into()
            })
            .unwrap_err()
            .1,
        a
    );
    assert_eq!(table.iter_by_group("red").count(), 3);
    assert_eq!(table.iter_by_cached_group("red").count(), 3);
    assert_eq!(table.get(idle), Some(&Event::Idle));
    assert_eq!(table.get_by_key(&3).unwrap().0, c);
    let error = table.replace(b, Event::Pair(1, "blue".into())).unwrap_err();
    assert_eq!(
        error.kind,
        microtable::ReplaceErrorKind::Unique { field: "key" }
    );
    assert_eq!(table.iter_by_group("red").count(), 3);
    table.replace(b, Event::Pair(2, "blue".into())).unwrap();
    table.replace(a, Event::Idle).unwrap();
    table
        .replace(
            idle,
            Event::Named {
                key: 1,
                group: "blue".into(),
            },
        )
        .unwrap();
    assert_eq!(table.get_by_key(&1).unwrap().0, idle);
    assert_eq!(table.iter_by_group("red").count(), 1);
    assert_eq!(table.iter_by_cached_group("blue").count(), 2);
    table.remove(c).unwrap();
    assert_eq!(table.iter_by_group("red").count(), 0);
    assert_eq!(table.iter_by_cached_group("red").count(), 0);
}
