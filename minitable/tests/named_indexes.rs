use minitable::{InsertErrorKind, ReplaceErrorKind, table};

macro_rules! member_row {
    ($row:ident, $table:ident, $make:ident, $group:ident $(, $option:meta)*) => {
        #[table(table = $table $(, $option)*)]
        #[index(name = tenant_handle, unique, fields(tenant, handle))]
        #[index(name = tenant_category, many, cached, fields(tenant, category))]
        #[index(name = normalized, unique, cached = false, key_type = String, key = |row| format!("{}:{}", row.tenant, row.handle.to_ascii_lowercase()))]
        #[index(name = category_key, many, cached = false, key_type = String, key = |row| format!("{}:{}", row.tenant, row.category))]
        #[index(name = cached_category, many, key_type = String, key = |row| format!("{}:{}", row.tenant, row.category))]
        #[derive(Debug, PartialEq, Eq)]
        struct $row {
            #[indexed(unique)]
            serial: u32,
            tenant: u32,
            handle: String,
            category: String,
        }

        fn $make(serial: u32, tenant: u32, handle: &str, category: &str) -> $row {
            $row {
                serial,
                tenant,
                handle: handle.into(),
                category: category.into(),
            }
        }

        fn $group(table: &$table, tenant: u32, category: &str) -> Vec<u32> {
            let mut result: Vec<_> = table
                .iter_by_tenant_category(&tenant, category)
                .map(|(_, row)| row.serial)
                .collect();
            result.sort_unstable();
            result
        }
    };
}

member_row!(Member, MemberTable, member, group);
member_row!(
    AppendMember,
    AppendMemberTable,
    append_member,
    append_group,
    append_only
);

#[derive(Debug, PartialEq, Eq)]
struct ItemPayload {
    email: String,
    serial: u32,
    kind: String,
}

fn item_payload(serial: u32, email: &str, kind: &str) -> ItemPayload {
    ItemPayload {
        email: email.into(),
        serial,
        kind: kind.into(),
    }
}

macro_rules! item_enum {
    ($row:ident $(, $option:meta)*) => {
        #[table($($option),*)]
        #[derive(Debug, PartialEq, Eq)]
        enum $row {
            #[index(name = email, unique, key_type = String, key = |payload| payload.email.clone())]
            #[index(name = serial, unique, cached = false, key_type = u32, key = |payload| payload.serial)]
            #[index(name = kind, many, key_type = String, key = |payload| payload.kind.clone())]
            #[index(name = live_kind, many, cached = false, key_type = String, key = |payload| payload.kind.clone())]
            Person(ItemPayload),
            #[index(name = email, unique, cached = false, key_type = String, key = |payload| payload.email.clone())]
            #[index(name = serial, unique, key_type = u32, key = |payload| payload.serial)]
            #[index(name = kind, many, cached = false, key_type = String, key = |payload| payload.kind.clone())]
            #[index(name = cached_kind, many, cached = true, key_type = String, key = |payload| payload.kind.clone())]
            Device(ItemPayload),
            Unindexed(()),
        }
    };
}

item_enum!(Item);
item_enum!(AppendItem, append_only);

#[test]
fn named_indexes_coexist_and_support_borrowed_queries() {
    macro_rules! check_enum {
        ($row:ident, $table:ident) => {{
            let mut table = $table::default();
            assert!(table.iter_person_by_kind("shared").next().is_none());
            assert!(table.iter_device_by_kind("shared").next().is_none());
            let a = table
                .insert($row::Person(item_payload(1, "same", "shared")))
                .unwrap();
            let b = table
                .insert($row::Device(item_payload(1, "same", "shared")))
                .unwrap();
            let unindexed = table.insert($row::Unindexed(())).unwrap();
            assert_eq!(
                table.get_person_by_email("same"),
                Some((a, table.get(a).unwrap()))
            );
            assert_eq!(
                table.get_device_by_email("same"),
                Some((b, table.get(b).unwrap()))
            );
            assert_eq!(table.get_person_by_serial(&1).unwrap().0, a);
            assert_eq!(table.get_device_by_serial(&1).unwrap().0, b);
            macro_rules! singleton {
                ($iter:expr, $id:expr) => {{
                    let mut iter = $iter;
                    assert_eq!(iter.next().unwrap().0, $id);
                    assert!(iter.next().is_none());
                    assert!(iter.next().is_none());
                }};
            }
            singleton!(table.iter_person_by_kind("shared"), a);
            singleton!(table.iter_person_by_live_kind("shared"), a);
            singleton!(table.iter_device_by_kind("shared"), b);
            singleton!(table.iter_device_by_cached_kind("shared"), b);
            let c = table
                .insert($row::Person(item_payload(2, "person-only", "shared")))
                .unwrap();
            let d = table
                .insert($row::Device(item_payload(2, "device-only", "shared")))
                .unwrap();
            assert!(table.get_device_by_email("person-only").is_none());
            assert!(table.get_person_by_email("device-only").is_none());
            for iter in [
                table.iter_person_by_kind("shared").collect::<Vec<_>>(),
                table.iter_person_by_live_kind("shared").collect(),
            ] {
                let mut ids: Vec<_> = iter
                    .into_iter()
                    .map(|(id, row)| {
                        assert!(matches!(row, $row::Person(_)));
                        id
                    })
                    .collect();
                ids.sort_unstable();
                assert_eq!(ids, [a, c]);
            }
            for iter in [
                table.iter_device_by_kind("shared").collect::<Vec<_>>(),
                table.iter_device_by_cached_kind("shared").collect(),
            ] {
                let mut ids: Vec<_> = iter
                    .into_iter()
                    .map(|(id, row)| {
                        assert!(matches!(row, $row::Device(_)));
                        id
                    })
                    .collect();
                ids.sort_unstable();
                assert_eq!(ids, [b, d]);
            }
            for row in [
                $row::Person(item_payload(3, "same", "rejected")),
                $row::Person(item_payload(1, "unused", "rejected")),
                $row::Device(item_payload(3, "same", "rejected")),
                $row::Device(item_payload(1, "unused", "rejected")),
            ] {
                let error = table.insert(row).unwrap_err();
                assert!(matches!(error.kind, InsertErrorKind::Unique { .. }));
                assert_eq!(table.len(), 5);
                assert!(table.get_person_by_email("unused").is_none());
                assert!(table.get_device_by_email("unused").is_none());
                assert_eq!(table.iter_person_by_kind("rejected").count(), 0);
                assert_eq!(table.iter_device_by_kind("rejected").count(), 0);
            }
            assert_eq!(table.get(unindexed), Some(&$row::Unindexed(())));
            let matches = {
                let query = String::from("shared");
                table.iter_device_by_kind(query.as_str())
            };
            assert_eq!(matches.take(3).count(), 2);
            assert!(table.iter_device_by_cached_kind("missing").next().is_none());
        }};
    }
    check_enum!(Item, ItemTable);
    check_enum!(AppendItem, AppendItemTable);
    macro_rules! check {
        ($table:ty, $member:path, $group:path) => {{
            let mut table = <$table>::new();
            assert!(table.is_empty());
            let a = table.insert($member(1, 1, "Ada", "red")).unwrap();
            macro_rules! check_singleton {
                ($iter:expr) => {{
                    let mut iter = $iter;
                    assert_eq!(iter.next().unwrap().0, a);
                    assert!(iter.next().is_none());
                    assert!(iter.next().is_none());
                }};
            }
            check_singleton!(table.iter_by_tenant_category(&1, "red"));
            check_singleton!(table.iter_by_category_key("1:red"));
            check_singleton!(table.iter_by_cached_category("1:red"));
            let b = table.insert($member(2, 2, "Ada", "red")).unwrap();
            table.insert($member(3, 1, "Grace", "red")).unwrap();
            assert_eq!(table.get_by_serial(&1).unwrap().0, a);
            assert_eq!(table.get_by_tenant_handle(&1, "Ada").unwrap().0, a);
            assert_eq!(table.get_by_tenant_handle(&2, "Ada").unwrap().0, b);
            assert!(table.get_by_tenant_handle(&3, "Ada").is_none());
            assert_eq!(table.get_by_normalized("1:ada").unwrap().0, a);
            assert_eq!($group(&table, 1, "red"), [1, 3]);
            assert_eq!($group(&table, 2, "red"), [2]);
            for (handle, index) in [("Ada", "tenant_handle"), ("aDA", "normalized")] {
                let error = table.insert($member(4, 1, handle, "blue")).unwrap_err();
                assert_eq!(error.kind, InsertErrorKind::Unique { field: index });
                assert_eq!(error.row.handle, handle);
                assert_eq!(table.len(), 3);
                assert!(table.get_by_serial(&4).is_none());
                assert!($group(&table, 1, "blue").is_empty());
            }
            macro_rules! check_iter {
                ($iter:expr, $expected:expr) => {{
                    let mut iter = $iter;
                    let mut serials = Vec::new();
                    for _ in 0..$expected.len() {
                        serials.push(iter.next().unwrap().1.serial);
                    }
                    assert!(iter.next().is_none());
                    assert!(iter.next().is_none());
                    serials.sort_unstable();
                    assert_eq!(serials, $expected);
                }};
            }
            check_iter!(table.iter_by_tenant_category(&1, "red"), [1, 3]);
            check_iter!(table.iter_by_category_key("1:red"), [1, 3]);
            check_iter!(table.iter_by_cached_category("1:red"), [1, 3]);
            check_iter!(table.iter_by_tenant_category(&2, "red"), [2]);
            check_iter!(table.iter_by_category_key("2:red"), [2]);
            check_iter!(table.iter_by_cached_category("2:red"), [2]);
            check_iter!(table.iter_by_tenant_category(&1, "absent"), [] as [u32; 0]);
            check_iter!(table.iter_by_category_key("absent"), [] as [u32; 0]);
            check_iter!(table.iter_by_cached_category("absent"), [] as [u32; 0]);
            let iter = {
                let query = String::from("red");
                let tenant = 1;
                table.iter_by_tenant_category(&tenant, query.as_str())
            };
            assert_eq!(iter.take(3).count(), 2);
        }};
    }
    check!(MemberTable, member, group);
    check!(AppendMemberTable, append_member, append_group);
}

#[test]
fn named_constraints_leave_all_indexes_unchanged_on_error() {
    let mut table = MemberTable::new();
    let a = table.insert(member(1, 1, "Ada", "red")).unwrap();
    let b = table.insert(member(2, 1, "Grace", "red")).unwrap();
    for (handle, index) in [("Ada", "tenant_handle"), ("aDA", "normalized")] {
        let row = member(3, 1, handle, "blue");
        let error = table.insert(row).unwrap_err();
        assert_eq!(error.kind, InsertErrorKind::Unique { field: index });
        assert_eq!(error.row.handle, handle);
        let error = table.replace(b, member(3, 1, handle, "blue")).unwrap_err();
        assert_eq!(error.kind, ReplaceErrorKind::Unique { field: index });
        assert!(table.get_by_serial(&3).is_none());
        assert_eq!(group(&table, 1, "red"), [1, 2]);
        assert!(group(&table, 1, "blue").is_empty());
        assert_eq!(table.get_by_tenant_handle(&1, "Ada").unwrap().0, a);
        assert_eq!(table.get_by_tenant_handle(&1, "Grace").unwrap().0, b);
    }
}

#[test]
fn replacement_and_removal_update_composite_and_computed_lists() {
    let mut items = ItemTable::new();
    let a = items
        .insert(Item::Person(item_payload(1, "same", "red")))
        .unwrap();
    let b = items
        .insert(Item::Person(item_payload(2, "other", "red")))
        .unwrap();
    let d = items
        .insert(Item::Device(item_payload(1, "same", "red")))
        .unwrap();
    // Both target-variant unique constraints must be checked before old links detach.
    for row in [
        Item::Device(item_payload(3, "same", "blue")),
        Item::Device(item_payload(1, "unused", "blue")),
    ] {
        let error = items.replace(b, row).unwrap_err();
        assert!(matches!(error.kind, ReplaceErrorKind::Unique { .. }));
        assert_eq!(
            items.get(b),
            Some(&Item::Person(item_payload(2, "other", "red")))
        );
        assert_eq!(items.get_person_by_email("other").unwrap().0, b);
        assert_eq!(items.get_person_by_serial(&2).unwrap().0, b);
        assert_eq!(items.get_device_by_email("same").unwrap().0, d);
        assert_eq!(items.iter_person_by_kind("red").count(), 2);
        assert_eq!(items.iter_person_by_live_kind("red").count(), 2);
        assert_eq!(items.iter_device_by_kind("red").count(), 1);
        assert_eq!(items.iter_device_by_cached_kind("red").count(), 1);
        assert_eq!(items.iter_device_by_kind("blue").count(), 0);
        assert_eq!(items.iter_device_by_cached_kind("blue").count(), 0);
        assert!(items.get_device_by_email("unused").is_none());
    }
    assert_eq!(
        items
            .replace(b, Item::Device(item_payload(2, "other", "red")))
            .unwrap(),
        Item::Person(item_payload(2, "other", "red"))
    );
    assert!(items.get_person_by_email("other").is_none());
    assert!(items.get_person_by_serial(&2).is_none());
    assert_eq!(items.get_device_by_email("other").unwrap().0, b);
    assert_eq!(items.iter_person_by_kind("red").count(), 1);
    assert_eq!(items.iter_person_by_live_kind("red").count(), 1);
    assert_eq!(items.iter_device_by_kind("red").count(), 2);
    assert_eq!(items.iter_device_by_cached_kind("red").count(), 2);
    items
        .replace(b, Item::Person(item_payload(2, "other", "blue")))
        .unwrap();
    assert!(items.get_device_by_email("other").is_none());
    assert!(items.get_device_by_serial(&2).is_none());
    assert_eq!(items.iter_device_by_kind("red").count(), 1);
    assert_eq!(items.iter_device_by_cached_kind("red").count(), 1);
    assert_eq!(items.iter_person_by_kind("blue").next().unwrap().0, b);
    assert_eq!(items.iter_person_by_live_kind("blue").next().unwrap().0, b);
    items.replace(b, Item::Unindexed(())).unwrap();
    assert!(items.get_person_by_email("other").is_none());
    assert_eq!(items.iter_person_by_kind("blue").count(), 0);
    assert_eq!(items.iter_person_by_live_kind("blue").count(), 0);
    items.remove(a);
    items.remove(d);
    assert_eq!(items.iter_person_by_kind("red").count(), 0);
    assert_eq!(items.iter_person_by_live_kind("red").count(), 0);
    assert_eq!(items.iter_device_by_kind("red").count(), 0);
    assert_eq!(items.iter_device_by_cached_kind("red").count(), 0);
    items
        .replace(b, Item::Device(item_payload(1, "same", "red")))
        .unwrap();
    assert_eq!(items.get_device_by_email("same").unwrap().0, b);
    items.remove(b);
    assert!(items.is_empty());

    let mut table = MemberTable::new();
    let a = table.insert(member(1, 1, "Ada", "red")).unwrap();
    let b = table.insert(member(2, 1, "Grace", "red")).unwrap();
    let c = table.insert(member(3, 1, "Linus", "red")).unwrap();
    assert_eq!(
        table.replace(b, member(4, 2, "GRACE", "blue")).unwrap(),
        member(2, 1, "Grace", "red")
    );
    assert_eq!(group(&table, 1, "red"), [1, 3]);
    assert_eq!(group(&table, 2, "blue"), [4]);
    assert_eq!(table.get_by_tenant_handle(&2, "GRACE").unwrap().0, b);
    assert!(table.get_by_tenant_handle(&1, "Grace").is_none());
    assert!(table.get_by_normalized("1:grace").is_none());
    assert_eq!(table.get_by_normalized("2:grace").unwrap().0, b);
    // Unchanged computed keys and composite keys are valid replacements.
    table.replace(b, member(4, 2, "grace", "blue")).unwrap();
    assert_eq!(table.iter_by_category_key("2:blue").count(), 1);
    table.remove(a); // tail
    table.remove(c); // singleton
    assert!(group(&table, 1, "red").is_empty());
    assert_eq!(table.iter_by_category_key("1:red").count(), 0);
    table.remove(b);
    assert!(table.get_by_normalized("2:grace").is_none());
    assert_eq!(table.iter_by_category_key("2:blue").count(), 0);
    let reused = table.insert(member(5, 1, "Ada", "red")).unwrap();
    assert_eq!(table.get_by_tenant_handle(&1, "Ada").unwrap().0, reused);
    assert_eq!(group(&table, 1, "red"), [5]);
}

macro_rules! generic_row {
    ($row:ident $(, $option:meta)*) => {
        #[table($($option),*)]
        #[index(name = pair, unique, fields(left, right))]
        #[index(name = borrowed, many, key_type = &'a str, key = |row| row.label)]
        struct $row<'a, T, const N: usize>
        where
            T: PartialEq,
        {
            left: T,
            right: String,
            label: &'a str,
            data: [u8; N],
        }
    };
}

generic_row!(Generic);
generic_row!(AppendGeneric, append_only = true);

struct GenericPayload<'a, T, const N: usize> {
    key: T,
    label: &'a str,
    data: [u8; N],
}

macro_rules! generic_enum {
    ($row:ident $(, $option:meta)*) => {
        #[table($($option),*)]
        enum $row<'a, T, const N: usize>
        where
            T: Clone + Eq + std::hash::Hash,
        {
            #[index(name = key, unique, key_type = T, key = |payload| payload.key.clone())]
            #[index(name = borrowed, many, key_type = &'a str, key = |payload| payload.label)]
            Value(GenericPayload<'a, T, N>),
            #[index(name = key, unique, cached = false, key_type = T, key = |payload| payload.key.clone())]
            #[index(name = borrowed, many, cached = false, key_type = &'a str, key = |payload| payload.label)]
            Alternate(GenericPayload<'a, T, N>),
        }
    };
}

generic_enum!(GenericItem);
generic_enum!(AppendGenericItem, append_only);

#[test]
fn named_indexes_support_generics_and_borrowed_computed_outputs() {
    let borrowed = String::from("enum label");
    macro_rules! check_enum {
        ($row:ident, $table:ident) => {{
            let mut table = $table::<String, 3>::new();
            let a = table
                .insert($row::Value(GenericPayload {
                    key: "shared".into(),
                    label: &borrowed,
                    data: [1, 2, 3],
                }))
                .ok()
                .unwrap();
            let b = table
                .insert($row::Alternate(GenericPayload {
                    key: "shared".into(),
                    label: &borrowed,
                    data: [4, 5, 6],
                }))
                .ok()
                .unwrap();
            assert_eq!(table.get_value_by_key("shared").unwrap().0, a);
            assert_eq!(table.get_alternate_by_key("shared").unwrap().0, b);
            let matches = {
                let query = String::from("enum label");
                table.iter_value_by_borrowed(query.as_str())
            };
            let mut matches = matches;
            let (id, row) = matches.next().unwrap();
            assert_eq!(id, a);
            match row {
                $row::Value(payload) => assert_eq!(payload.data, [1, 2, 3]),
                _ => panic!("wrong variant"),
            }
            assert!(matches.next().is_none());
            assert!(matches.next().is_none());
            let mut matches = table.iter_alternate_by_borrowed("enum label");
            let (id, row) = matches.next().unwrap();
            assert_eq!(id, b);
            match row {
                $row::Alternate(payload) => assert_eq!(payload.data, [4, 5, 6]),
                _ => panic!("wrong variant"),
            }
            assert!(matches.next().is_none());
            assert!(matches.next().is_none());
            assert!(table.iter_value_by_borrowed("absent").next().is_none());
            assert!(table.iter_alternate_by_borrowed("absent").next().is_none());
        }};
    }
    check_enum!(GenericItem, GenericItemTable);
    check_enum!(AppendGenericItem, AppendGenericItemTable);

    #[derive(PartialEq, Eq, Hash)]
    struct Key(u32); // Composite field keys do not need Clone.

    let label = String::from("label");
    macro_rules! check {
        ($row:ident, $table:ident) => {{
            let mut table = $table::<Key, 3>::new();
            let id = table
                .insert($row {
                    left: Key(7),
                    right: "right".into(),
                    label: &label,
                    data: [1, 2, 3],
                })
                .ok()
                .unwrap();
            assert_eq!(table.get_by_pair(&Key(7), "right").unwrap().0, id);
            assert_eq!(
                table.iter_by_borrowed("label").next().unwrap().1.data,
                [1, 2, 3]
            );
            assert!(table.contains(id));
            let mut matches = table.iter_by_borrowed("label");
            assert_eq!(matches.next().unwrap().0, id);
            assert!(matches.next().is_none());
            assert!(matches.next().is_none());
            assert!(table.iter_by_borrowed("absent").next().is_none());
            drop(matches);
            (table, id)
        }};
    }
    let (mut table, id) = check!(Generic, GenericTable);
    assert!(table.remove(id).is_some());
    let _ = check!(AppendGeneric, AppendGenericTable);
}

macro_rules! computed_row {
    ($row:ident, $compute:ident $(, $option:meta)*) => {
        #[table($($option),*)]
        #[index(name = calculated, unique, key_type = ComputedKey, key = $compute)]
        #[index(name = bucket, many, key_type = ComputedKey, key = $compute)]
        #[derive(Debug)]
        struct $row {
            value: u32,
            calls: std::rc::Rc<std::cell::Cell<usize>>,
        }

        fn $compute(row: &$row) -> ComputedKey {
            row.calls.set(row.calls.get() + 1);
            ComputedKey(row.value)
        }
    };
}

computed_row!(Computed, compute_once);
computed_row!(AppendComputed, append_compute_once, append_only);

#[derive(Debug)]
struct ComputedPayload {
    value: u32,
    calls: std::rc::Rc<std::cell::Cell<usize>>,
}

fn compute_payload(payload: &ComputedPayload) -> ComputedKey {
    payload.calls.set(payload.calls.get() + 1);
    ComputedKey(payload.value)
}

macro_rules! computed_enum {
    ($row:ident $(, $option:meta)*) => {
        #[table($($option),*)]
        #[derive(Debug)]
        enum $row {
            #[index(name = calculated, unique, key_type = ComputedKey, key = compute_payload)]
            #[index(name = bucket, many, cached = true, key_type = ComputedKey, key = compute_payload)]
            Value(ComputedPayload),
            #[index(name = calculated, unique, key_type = ComputedKey, key = compute_payload)]
            #[index(name = bucket, many, key_type = ComputedKey, key = compute_payload)]
            Alternate(ComputedPayload),
        }
    };
}

computed_enum!(ComputedItem);
computed_enum!(AppendComputedItem, append_only);

#[derive(Debug, PartialEq, Eq, Hash)]
struct ComputedKey(u32);

impl std::borrow::Borrow<u32> for ComputedKey {
    fn borrow(&self) -> &u32 {
        &self.0
    }
}

#[test]
fn computed_keys_are_cached_for_removal_and_old_replacement_keys() {
    macro_rules! check_enum {
        ($row:ident, $table:ident) => {{
            let calls = std::rc::Rc::new(std::cell::Cell::new(0));
            let mut table = $table::new();
            for value in 0..64 {
                let a = table
                    .insert($row::Value(ComputedPayload {
                        value,
                        calls: calls.clone(),
                    }))
                    .unwrap();
                let b = table
                    .insert($row::Alternate(ComputedPayload {
                        value,
                        calls: calls.clone(),
                    }))
                    .unwrap();
                assert_eq!(calls.get(), 4 * (value + 1) as usize);
                assert_eq!(table.get_value_by_calculated(&value).unwrap().0, a);
                assert_eq!(table.get_alternate_by_calculated(&value).unwrap().0, b);
                assert_eq!(table.iter_value_by_bucket(&value).count(), 1);
                assert_eq!(table.iter_alternate_by_bucket(&value).count(), 1);
                assert_eq!(calls.get(), 4 * (value + 1) as usize);
            }
            assert!(table.iter_value_by_bucket(&64).next().is_none());
            assert!(table.iter_alternate_by_bucket(&64).next().is_none());
            assert_eq!(calls.get(), 256);
            (table, calls)
        }};
    }
    let (mut items, enum_calls) = check_enum!(ComputedItem, ComputedItemTable);
    let _ = check_enum!(AppendComputedItem, AppendComputedItemTable);
    let id = items.get_value_by_calculated(&0).unwrap().0;
    items
        .replace(
            id,
            ComputedItem::Alternate(ComputedPayload {
                value: 64,
                calls: enum_calls.clone(),
            }),
        )
        .unwrap();
    assert_eq!(enum_calls.get(), 258);
    assert!(items.get_value_by_calculated(&0).is_none());
    assert_eq!(items.iter_value_by_bucket(&0).count(), 0);
    assert_eq!(items.get_alternate_by_calculated(&64).unwrap().0, id);
    assert_eq!(items.iter_alternate_by_bucket(&64).count(), 1);
    items.remove(id);
    let ids: Vec<_> = items.iter().map(|(id, _)| id).collect();
    for id in ids {
        items.remove(id);
    }
    assert!(items.is_empty());
    assert_eq!(enum_calls.get(), 258);

    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut table = ComputedTable::new();
    let id = table
        .insert(Computed {
            value: 1,
            calls: calls.clone(),
        })
        .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(table.get_by_calculated(&1).unwrap().0, id);
    assert_eq!(table.iter_by_bucket(&1).count(), 1);
    assert_eq!(calls.get(), 2);
    table
        .replace(
            id,
            Computed {
                value: 2,
                calls: calls.clone(),
            },
        )
        .unwrap();
    assert_eq!(calls.get(), 4);

    let error = table
        .insert(Computed {
            value: 2,
            calls: calls.clone(),
        })
        .unwrap_err();
    assert_eq!(
        error.kind,
        InsertErrorKind::Unique {
            field: "calculated"
        }
    );
    assert_eq!(calls.get(), 6);
    table
        .replace(
            id,
            Computed {
                value: 2,
                calls: calls.clone(),
            },
        )
        .unwrap();
    assert_eq!(calls.get(), 8);

    // Grow both caches. Rehash and reads must not call the key functions.
    let mut others = Vec::new();
    for value in 3..67 {
        others.push(
            table
                .insert(Computed {
                    value,
                    calls: calls.clone(),
                })
                .unwrap(),
        );
        assert_eq!(calls.get(), 8 + 2 * (value - 2) as usize);
        assert!(table.get_by_calculated(&value).is_some());
        assert_eq!(table.iter_by_bucket(&value).count(), 1);
        assert_eq!(calls.get(), 8 + 2 * (value - 2) as usize);
    }
    for other in others {
        table.remove(other);
    }
    table.remove(id);
    assert_eq!(calls.get(), 136);
    let error = table
        .replace(
            id,
            Computed {
                value: 3,
                calls: calls.clone(),
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ReplaceErrorKind::NotFound);
    assert_eq!(calls.get(), 136);
    assert!(table.get_by_calculated(&1).is_none());
    assert!(table.get_by_calculated(&2).is_none());
    assert_eq!(table.iter_by_bucket(&2).count(), 0);

    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut append = AppendComputedTable::default();
    for value in 0..64 {
        let id = append
            .insert(AppendComputed {
                value,
                calls: calls.clone(),
            })
            .unwrap();
        assert_eq!(calls.get(), 2 * (value + 1) as usize);
        assert_eq!(append.get_by_calculated(&value).unwrap().0, id);
        let mut iter = append.iter_by_bucket(&value);
        assert_eq!(iter.next().unwrap().0, id);
        assert!(iter.next().is_none());
        assert!(iter.next().is_none());
        assert_eq!(calls.get(), 2 * (value + 1) as usize);
    }
    let error = append
        .insert(AppendComputed {
            value: 0,
            calls: calls.clone(),
        })
        .unwrap_err();
    assert_eq!(
        error.kind,
        InsertErrorKind::Unique {
            field: "calculated"
        }
    );
    assert_eq!(calls.get(), 130);
    assert_eq!(append.len(), 64);
    assert!(append.iter_by_bucket(&64).next().is_none());
    assert_eq!(calls.get(), 130);
}

#[test]
fn randomized_named_index_operations_match_a_scan_model() {
    let mut table = MemberTable::new();
    let mut model = std::collections::HashMap::<usize, Member>::new();
    let mut history = Vec::new();
    let mut seed = 19u64;
    for _ in 0..1500 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let serial = ((seed >> 32) % 25) as u32;
        let tenant = ((seed >> 40) % 3) as u32;
        let handle = ["Ada", "aDA", "Grace", "Linus", "grace"][(seed >> 48) as usize % 5];
        let category = ["red", "blue", "green"][(seed >> 56) as usize % 3];
        let chosen = (!history.is_empty()).then(|| history[(seed >> 16) as usize % history.len()]);
        let conflicting = |except| {
            model.iter().any(|(&id, old)| {
                Some(id) != except
                    && (old.serial == serial
                        || (old.tenant == tenant && old.handle.eq_ignore_ascii_case(handle)))
            })
        };
        match (seed % 3, chosen) {
            (0, _) | (_, None) => {
                let conflict = conflicting(None);
                match table.insert(member(serial, tenant, handle, category)) {
                    Ok(id) => {
                        assert!(!conflict);
                        model.insert(id, member(serial, tenant, handle, category));
                        history.push(id);
                    }
                    Err(_) => assert!(conflict),
                }
            }
            (1, Some(id)) => {
                assert_eq!(table.remove(id), model.remove(&id));
            }
            (_, Some(id)) => {
                let conflict = conflicting(Some(id));
                match table.replace(id, member(serial, tenant, handle, category)) {
                    Ok(old) => {
                        assert!(!conflict);
                        assert_eq!(
                            model.insert(id, member(serial, tenant, handle, category)),
                            Some(old)
                        );
                    }
                    Err(_) => assert!(conflict || !model.contains_key(&id)),
                }
            }
        }
        assert_eq!(table.len(), model.len());
        for (&id, row) in &model {
            assert_eq!(table.get(id), Some(row));
            assert_eq!(table.get_by_serial(&row.serial).unwrap().0, id);
            assert_eq!(
                table
                    .get_by_tenant_handle(&row.tenant, row.handle.as_str())
                    .unwrap()
                    .0,
                id
            );
            let normalized = format!("{}:{}", row.tenant, row.handle.to_ascii_lowercase());
            assert_eq!(table.get_by_normalized(normalized.as_str()).unwrap().0, id);
        }
        for tenant in 0..3 {
            for category in ["red", "blue", "green"] {
                let mut expected: Vec<_> = model
                    .values()
                    .filter(|row| row.tenant == tenant && row.category == category)
                    .map(|row| row.serial)
                    .collect();
                expected.sort_unstable();
                assert_eq!(group(&table, tenant, category), expected);
                let key = format!("{tenant}:{category}");
                let mut actual: Vec<_> = table
                    .iter_by_category_key(key.as_str())
                    .map(|(_, row)| row.serial)
                    .collect();
                actual.sort_unstable();
                assert_eq!(actual, expected);
            }
        }
    }
}
