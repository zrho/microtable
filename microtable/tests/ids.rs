use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[derive(Debug)]
struct Row(u16);
microtable::table! {
    struct Rows([Row]);
    #[id]
    pub struct RowId(u8);
    #[index(unique)]
    fn key(row: &Row) -> u16 { row.0 }
}
microtable::table! {
    #[table(append_only)]
    struct AppendRows([Row]);
    #[id]
    struct AppendId(u8);
    #[index(unique, cached)]
    fn key(row: &Row) -> u16 { row.0 }
}
microtable::table! {
    struct UsizeRows([Row]);
}
microtable::table! {
    struct WideRows([Row]);
    #[id]
    struct WideId(usize);
}

#[test]
fn generated_id_conversions_traits_and_nonzero_niche() {
    fn traits<T: Copy + Clone + std::fmt::Debug + Eq + Ord + std::hash::Hash>() {}
    traits::<RowId>();
    assert_eq!(std::mem::size_of::<RowId>(), 1);
    assert_eq!(std::mem::size_of::<Option<RowId>>(), 1);
    assert_eq!(
        std::mem::size_of::<Option<WideId>>(),
        std::mem::size_of::<usize>()
    );
    let mut ids = HashSet::new();
    for index in 0..255 {
        let id = RowId::new(index);
        assert_eq!(id.index(), index);
        assert_eq!(usize::from(id), index);
        assert_eq!(RowId::try_new(index), Some(id));
        assert_eq!(RowId::try_from(index).unwrap(), id);
        assert!(ids.insert(id));
        if index > 0 {
            assert!(RowId::new(index - 1) < id);
        }
    }
    for index in [255, 256, usize::MAX] {
        assert_eq!(RowId::try_new(index), None);
        assert!(RowId::try_from(index).is_err());
        assert!(catch_unwind(|| RowId::new(index)).is_err());
    }
    assert_eq!(WideId::new(0).index(), 0);
    assert_eq!(WideId::new(usize::MAX - 1).index(), usize::MAX - 1);
    assert_eq!(WideId::try_new(usize::MAX), None);
}

#[test]
fn no_id_declaration_uses_usize_and_reuses_slot_zero() {
    let mut table = UsizeRows::new();
    let first: usize = table.insert(Row(1)).unwrap();
    assert_eq!(first, 0);
    assert_eq!(table.remove(first).unwrap().0, 1);
    let reused: usize = table.insert(Row(2)).unwrap();
    assert_eq!(reused, first);
    assert_eq!(table.get(reused).unwrap().0, 2);
}

#[test]
fn mutable_u8_exhaustion_is_checked_after_unique_validation_and_slots_reuse() {
    let mut table = Rows::new();
    for index in 0..255 {
        let predicted = table.next_id();
        assert_eq!(predicted.index(), index);
        assert_eq!(table.insert(Row(index as u16)).unwrap(), predicted);
    }
    assert_eq!(table.len(), 255);
    assert!(catch_unwind(AssertUnwindSafe(|| table.next_id())).is_err());
    // A conflict must return the row, not panic on exhausted storage.
    let error = table.insert(Row(0)).unwrap_err();
    assert_eq!(error.0.0, 0);
    assert_eq!(error.1, RowId::new(0));
    assert!(catch_unwind(AssertUnwindSafe(|| table.insert(Row(300)))).is_err());
    assert_eq!(table.len(), 255);
    assert!(table.get_by_key(&300).is_none());
    for index in 0..255 {
        assert_eq!(
            table.get_by_key(&(index as u16)).unwrap().0,
            RowId::new(index)
        );
    }
    // Replacement does not need a new ID even at capacity.
    table.replace(RowId::new(100), Row(300)).unwrap();
    assert_eq!(table.get_by_key(&300).unwrap().0, RowId::new(100));
    table.remove(RowId::new(12)).unwrap();
    table.remove(RowId::new(254)).unwrap();
    assert_eq!(table.next_id(), RowId::new(254));
    assert_eq!(table.insert(Row(301)).unwrap(), RowId::new(254));
    assert_eq!(table.next_id(), RowId::new(12));
    assert_eq!(table.insert(Row(302)).unwrap(), RowId::new(12));
    assert_eq!(table.len(), 255);
    assert!(catch_unwind(AssertUnwindSafe(|| table.insert(Row(303)))).is_err());
    assert!(table.get_by_key(&303).is_none());
}

#[test]
fn append_u8_exhaustion_validates_uniqueness_before_capacity() {
    let mut table = AppendRows::new();
    for index in 0..255 {
        assert_eq!(table.next_id(), AppendId::new(index));
        assert_eq!(
            table.insert(Row(index as u16)).unwrap(),
            AppendId::new(index)
        );
    }
    assert_eq!(table.insert(Row(10)).unwrap_err().1, AppendId::new(10));
    assert!(catch_unwind(AssertUnwindSafe(|| table.insert(Row(300)))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| table.next_id())).is_err());
    assert_eq!(table.len(), 255);
    assert_eq!(table.iter().count(), 255);
    assert!(table.get_by_key(&300).is_none());
    for index in 0..255 {
        assert_eq!(table.get_by_key(&(index as u16)).unwrap().0.index(), index);
    }
}
