//! This create provides an indexed, queryable column-based storage system.
//!
//! The storage system is, fundamentally, row-based storage, where all rows have the same number of
//! columns. All columns are the same "type", but given that they can be enum types, you can
//! effectively use differently typed values. Data is stored in a straightforward `Vec<Vec<T>>`,
//! where the outermost `Vec` is dynamically sized (and may be re-allocated as more rows come in),
//! whereas the innermost `Vec` is expected to never change.
//!
//! What makes this crate interesting is that it also allows you to place indices on columns for
//! fast lookups. These indices are automatically updates whenever the dataset changes, so that
//! queries continue to return correct results. Indices should conform to either the
//! `EqualityIndex` trait or the `RangeIndex` trait. As you would expect, the former allows
//! speeding up exact lookups, whereas the latter can also perform efficient range queries.
//!
//! Queries are performed over the dataset by calling `find` with a set of `Condition`s that will
//! be `AND`ed together. `OR` is currently not supported --- issue multiple quieries instead. Each
//! `Condition` represents a value comparison against the value in a single column. The system
//! automatically picks what index to use to satisfy the query, using a heuristic based on the
//! expected number of rows returned for that column for each index.
//!
//! # Known limitations
//!
//!  - The set of match operations is currently fairly limited.
//!  - The system currently provides an append-only abstraction (i.e., no delete or edit).

#![deny(missing_docs)]
#![feature(btree_range, collections_bound)]

use std::collections::HashMap;

/// The `cmp` module holds the mechanisms needed to compare values and express conditionals.
pub mod cmp;
pub use cmp::Comparison;
pub use cmp::Condition;
pub use cmp::Value;

/// The `idx` module described the traits indexers must adhere to, and implements sensible default
/// indexers.
pub mod idx;
pub use idx::EqualityIndex;
pub use idx::RangeIndex;
pub use idx::Index;

/// A `Store` is the main storage unit in shortcut. It keeps track of all the rows of data, as well
/// as what indices are available. You will generally be accessing the `Store` either through the
/// `find` method (which lets you find rows that match a certain condition), or through the
/// `insert` method, which lets you add another row.
///
/// Note that the type used for the rows needs to be `Clone`. This is because the value is also
/// given to the index, which (currently) take a full value, not just a borrow. This *might* change
/// down the line, but it's tricky to get the lifetimes to work out, because the indices would then
/// be scoped by the lifetime of the `Store`.
pub struct Store<T: PartialOrd + Clone> {
    cols: usize,
    rows: Vec<Vec<T>>,
    indices: HashMap<usize, Index<T>>,
}

impl<T: PartialOrd + Clone> Store<T> {
    /// Allocate a new `Store` with the given number of columns. The column count is checked in
    /// `insert` at runtime (bleh).
    pub fn new(cols: usize) -> Store<T> {
        Store {
            cols: cols,
            rows: Vec::new(),
            indices: HashMap::new(),
        }
    }

    /// Allocate a new `Store` with the given number of columns, and with room for the given number
    /// of rows. If you know roughly how many rows will be inserted, this will speed up insertion a
    /// fair amount, as it avoids needing to re-allocate the underlying `Vec` whenever it needs to
    /// grow. As with `new`, the column count is checked in `insert` at runtime.
    pub fn with_capacity(cols: usize, rows: usize) -> Store<T> {
        Store {
            cols: cols,
            rows: Vec::with_capacity(rows),
            indices: HashMap::new(),
        }
    }

    /// Returns an iterator that yields all rows matching all the given `Condition`s.
    ///
    /// This method will automatically determine what index to use to satisfy this query. It
    /// currently uses a fairly simple heuristic: it picks the index that: a) is over one of
    /// columns being filtered on; b) supports the operation for that filter; and c) has the lowest
    /// expected number of rows for a single value. This latter metric is generally the total
    /// number of rows divided by the number of entries in the index. See `EqualityIndex::estimate`
    /// for details.
    pub fn find<'a>(&'a self,
                    conds: &'a [cmp::Condition<T>])
                    -> Box<Iterator<Item = &'a [T]> + 'a> {

        use EqualityIndex;
        let best_idx = conds.iter()
            .enumerate()
            .filter_map(|(ci, c)| self.indices.get(&c.column).and_then(|idx| Some((ci, idx))))
            .filter(|&(ci, _)| {
                // does this index work for the operation in question?
                match conds[ci].cmp {
                    cmp::Comparison::Equal(cmp::Value::Const(..)) => true,
                    _ => false,
                }
            })
            .min_by_key(|&(_, idx)| idx.estimate());

        let iter = best_idx.and_then(|(ci, idx)| match conds[ci].cmp {
                cmp::Comparison::Equal(cmp::Value::Const(ref v)) => Some(idx.lookup(v)),
                _ => unreachable!(),
            })
            .unwrap_or_else(|| Box::new(0..self.rows.len()));

        Box::new(iter.map(move |rowi| &self.rows[rowi][..])
            .filter(move |row| conds.iter().all(|c| c.matches(row))))
    }

    /// Insert a new data row into the `Store`. The row **must** have the same number of columns as
    /// specified when the `Store` was created. If it does not, the code will panic with an
    /// assertion failure.
    ///
    /// Inserting a row has similar complexity to `Vec::push`, and *may* need to re-allocate the
    /// backing memory for the `Store`. The insertion also updates all maintained indices, which
    /// may also re-allocate.
    pub fn insert(&mut self, row: Vec<T>) {
        assert_eq!(row.len(), self.cols);
        let rowi = self.rows.len();
        for (column, idx) in self.indices.iter_mut() {
            use EqualityIndex;
            idx.index(row[*column].clone(), rowi);
        }
        self.rows.push(row);
    }

    /// Add an index on the given colum using the given indexer. The indexer *must*, at the very
    /// least, implement `EqualityIndex`. It *may* also implement other, more sophisticated,
    /// indexing strategies outlined in `Index`.
    ///
    /// When an index is added, it is immediately fed all rows in the current dataset. Thus, adding
    /// an index to a `Store` with many rows can be fairly costly. Keep this in mind!
    pub fn index<I: Into<Index<T>>>(&mut self, column: usize, indexer: I) {
        use EqualityIndex;
        let mut idx = indexer.into();

        // populate the new index
        for (rowi, row) in self.rows.iter().enumerate() {
            idx.index(row[column].clone(), rowi);
        }

        self.indices.insert(column, idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let mut store = Store::new(2);
        store.insert(vec!["a1", "a2"]);
        store.insert(vec!["b1", "b2"]);
        store.insert(vec!["c1", "c2"]);
        assert_eq!(store.find(&[]).count(), 3);
    }

    #[test]
    fn it_works_with_indices() {
        let mut store = Store::new(2);
        store.index(0, idx::HashIndex::new());
        store.insert(vec!["a1", "a2"]);
        store.insert(vec!["b1", "b2"]);
        store.insert(vec!["c1", "c2"]);
        assert_eq!(store.find(&[]).count(), 3);
    }

    #[test]
    fn it_filters() {
        let mut store = Store::new(2);
        store.insert(vec!["a", "x1"]);
        store.insert(vec!["a", "x2"]);
        store.insert(vec!["b", "x3"]);
        let cmp = [cmp::Condition {
                       column: 0,
                       cmp: cmp::Comparison::Equal(cmp::Value::Const("a")),
                   }];
        assert_eq!(store.find(&cmp)
                       .count(),
                   2);
        assert!(store.find(&cmp).all(|r| r[0] == "a"));
    }

    #[test]
    fn it_filters_with_indices() {
        let mut store = Store::new(2);
        store.index(0, idx::HashIndex::new());
        store.insert(vec!["a", "x1"]);
        store.insert(vec!["a", "x2"]);
        store.insert(vec!["b", "x3"]);
        let cmp = [cmp::Condition {
                       column: 0,
                       cmp: cmp::Comparison::Equal(cmp::Value::Const("a")),
                   }];
        assert_eq!(store.find(&cmp)
                       .count(),
                   2);
        assert!(store.find(&cmp).all(|r| r[0] == "a"));
    }

    #[test]
    fn it_filters_with_late_indices() {
        let mut store = Store::new(2);
        store.insert(vec!["a", "x1"]);
        store.insert(vec!["a", "x2"]);
        store.insert(vec!["b", "x3"]);
        store.index(0, idx::HashIndex::new());
        let cmp = [cmp::Condition {
                       column: 0,
                       cmp: cmp::Comparison::Equal(cmp::Value::Const("a")),
                   }];
        assert_eq!(store.find(&cmp)
                       .count(),
                   2);
        assert!(store.find(&cmp).all(|r| r[0] == "a"));
    }
}
