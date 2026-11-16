use minitable::{InsertErrorKind, ReplaceErrorKind, table};

#[table]
#[index(name = tenant_handle, unique, fields(tenant, handle))]
#[index(name = tenant_category, many, fields(tenant, category))]
#[index(name = normalized, unique, key_type = String, key = |row| format!("{}:{}", row.tenant, row.handle.to_ascii_lowercase()))]
#[index(name = category_key, many, key_type = String, key = category_key)]
#[derive(Debug, PartialEq, Eq)]
struct Member {
    #[indexed(unique)]
    serial: u32,
    tenant: u32,
    handle: String,
    category: String,
}

fn category_key(row: &Member) -> String {
    format!("{}:{}", row.tenant, row.category)
}

fn member(serial: u32, tenant: u32, handle: &str, category: &str) -> Member {
    Member {
        serial,
        tenant,
        handle: handle.into(),
        category: category.into(),
    }
}

fn group(table: &MemberTable, tenant: u32, category: &str) -> Vec<u32> {
    let mut result: Vec<_> = table
        .iter_by_tenant_category(&tenant, category)
        .map(|(_, row)| row.serial)
        .collect();
    result.sort_unstable();
    result
}

#[test]
fn named_indexes_coexist_and_support_borrowed_queries() {
    let mut table = MemberTable::new();
    let a = table.insert(member(1, 1, "Ada", "red")).unwrap();
    let b = table.insert(member(2, 2, "Ada", "red")).unwrap();
    table.insert(member(3, 1, "Grace", "red")).unwrap();
    assert_eq!(table.get_by_serial(&1).unwrap().0, a);
    assert_eq!(table.get_by_tenant_handle(&1, "Ada").unwrap().0, a);
    assert_eq!(table.get_by_tenant_handle(&2, "Ada").unwrap().0, b);
    assert!(table.get_by_tenant_handle(&3, "Ada").is_none());
    assert_eq!(table.get_by_normalized("1:ada").unwrap().0, a);
    assert_eq!(group(&table, 1, "red"), [1, 3]);
    assert_eq!(group(&table, 2, "red"), [2]);
    assert_eq!(table.iter_by_category_key("1:red").count(), 2);
    let iter = {
        let query = String::from("red");
        let tenant = 1;
        table.iter_by_tenant_category(&tenant, query.as_str())
    };
    assert_eq!(iter.count(), 2);
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

#[table]
#[index(name = pair, unique, fields(left, right))]
#[index(name = borrowed, many, key_type = &'a str, key = |row| row.label)]
struct Generic<'a, T, const N: usize>
where
    T: PartialEq,
{
    left: T,
    right: String,
    label: &'a str,
    data: [u8; N],
}

#[test]
fn named_indexes_support_generics_and_borrowed_computed_outputs() {
    let label = String::from("label");
    let mut table = GenericTable::<String, 3>::new();
    let id = table
        .insert(Generic {
            left: "left".into(),
            right: "right".into(),
            label: &label,
            data: [1, 2, 3],
        })
        .ok()
        .unwrap();
    assert_eq!(table.get_by_pair("left", "right").unwrap().0, id);
    assert_eq!(
        table.iter_by_borrowed("label").next().unwrap().1.data,
        [1, 2, 3]
    );
    assert!(table.remove(id).is_some());
}

#[table]
#[index(name = calculated, unique, key_type = u32, key = compute_once)]
#[index(name = bucket, many, key_type = u32, key = compute_once)]
#[derive(Debug)]
struct Computed {
    value: u32,
    calls: std::rc::Rc<std::cell::Cell<usize>>,
}

fn compute_once(row: &Computed) -> u32 {
    row.calls.set(row.calls.get() + 1);
    row.value
}

#[test]
fn computed_keys_are_cached_for_removal_and_old_replacement_keys() {
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
    table.remove(id);
    assert_eq!(calls.get(), 4);
    assert!(table.get_by_calculated(&1).is_none());
    assert!(table.get_by_calculated(&2).is_none());
    assert_eq!(table.iter_by_bucket(&2).count(), 0);
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
