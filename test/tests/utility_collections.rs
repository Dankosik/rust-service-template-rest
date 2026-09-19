//! Collection and constructor recipes; these are test-local examples, not domain APIs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use indexmap::{IndexMap, IndexSet};
use itertools::Itertools;

#[test]
fn group_and_count_without_parallel_bookkeeping() {
    let orders = [("alice", 101), ("bob", 102), ("alice", 103)];
    let grouped = orders.into_iter().into_group_map();
    assert_eq!(grouped["alice"], [101, 103]);
    assert_eq!(grouped["bob"], [102]);
    let counts = orders.into_iter().map(|(customer, _)| customer).counts();
    assert_eq!(counts["alice"], 2);
    assert_eq!(counts["bob"], 1);
}

#[test]
fn deduplicate_in_first_seen_order() {
    let ids = [12, 7, 12, 3, 7];
    assert_eq!(ids.into_iter().unique().collect_vec(), [12, 7, 3]);
    let mut selected: IndexSet<_> = ids.into_iter().collect();
    assert!(!selected.insert(7));
    assert!(selected.shift_remove(&7));
    assert_eq!(selected.into_iter().collect_vec(), [12, 3]);
}

#[test]
fn ordered_map_replaces_values_without_reordering_other_keys() {
    let mut values = IndexMap::from([("first", 1), ("second", 2), ("third", 3)]);
    assert_eq!(values.insert("second", 20), Some(2));
    assert_eq!(values.keys().copied().collect_vec(), ["first", "second", "third"]);
    // shift_remove, not swap_remove: relative order is part of this example.
    assert_eq!(values.shift_remove("first"), Some(1));
    assert_eq!(values.into_iter().collect_vec(), [("second", 20), ("third", 3)]);
}

#[derive(Debug, derive_more::Display, derive_more::AsRef)]
struct NonEmptyName(String);

impl TryFrom<&str> for NonEmptyName {
    type Error = &'static str;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let value = value.trim();
        if value.is_empty() {
            return Err("name must not be empty");
        }
        Ok(Self(value.to_owned()))
    }
}

#[test]
fn derived_traits_do_not_bypass_checked_construction() {
    assert!(NonEmptyName::try_from("  ").is_err());
    let name = NonEmptyName::try_from("  report  ").unwrap();
    assert_eq!(name.to_string(), "report");
    let inner: &String = name.as_ref();
    assert_eq!(inner, "report");
}

#[derive(Debug, bon::Builder)]
struct ExportOptions {
    name: String,
    #[builder(default = 100)]
    batch_size: usize,
    destination: Option<std::path::PathBuf>,
}

#[test]
fn a_builder_keeps_required_inputs_and_optional_defaults_distinct() {
    let defaulted = ExportOptions::builder().name("orders".to_owned()).build();
    assert_eq!(defaulted.name, "orders");
    assert_eq!(defaulted.batch_size, 100);
    assert!(defaulted.destination.is_none());
    let configured = ExportOptions::builder()
        .name("orders".to_owned())
        .batch_size(256)
        .destination("orders.csv".into())
        .build();
    assert_eq!(configured.batch_size, 256);
    assert_eq!(configured.destination.unwrap(), std::path::Path::new("orders.csv"));
}
