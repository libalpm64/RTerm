//! Defines the Row type which makes up lines in the grid.

use std::cmp::{max, min};
use std::ops::{Index, IndexMut, Range, RangeFrom, RangeFull, RangeTo, RangeToInclusive};
use std::sync::OnceLock;
use std::{mem, ptr, slice};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize, Serializer, ser::SerializeStruct};

use crate::grid::{AsciiCodec, GridCell};
use crate::index::Column;
use crate::term::cell::ResetDiscriminant;

/// A row in the grid.
#[derive(Default, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Deserialize))]
#[cfg_attr(feature = "serde", serde(bound(deserialize = "T: Deserialize<'de>")))]
pub struct Row<T> {
    inner: Vec<T>,
    #[cfg_attr(feature = "serde", serde(skip))]
    packed: Option<PackedRow<T>>,
    /// Optional command annotation owned by the terminal front end.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub command_mark: Option<std::num::NonZeroU64>,

    /// Maximum number of occupied entries.
    ///
    /// This is the upper bound on the number of elements in the row, which have been modified
    /// since the last reset. All cells after this point are guaranteed to be equal.
    pub(crate) occ: usize,
}

#[derive(Clone, Debug)]
struct PackedRow<T> {
    bytes: Vec<u8>,
    columns: usize,
    codec: AsciiCodec<T>,
    // Range indexing promises a contiguous slice. Rendering and selection use individual cells.
    expanded: OnceLock<Vec<T>>,
}

#[cfg(feature = "serde")]
impl<T: Serialize> Serialize for Row<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Row", 2)?;
        state.serialize_field("inner", self.cells())?;
        state.serialize_field("occ", &self.occ)?;
        state.end()
    }
}

impl<T: PartialEq> PartialEq for Row<T> {
    fn eq(&self, other: &Self) -> bool {
        self.cells() == other.cells()
    }
}

impl<T: Default> Row<T> {
    /// Create a new terminal row.
    ///
    /// Ideally the `template` should be `Copy` in all performance sensitive scenarios.
    pub fn new(columns: usize) -> Row<T> {
        debug_assert!(columns >= 1);

        let mut inner: Vec<T> = Vec::with_capacity(columns);

        // This is a slightly optimized version of `std::vec::Vec::resize`.
        unsafe {
            let mut ptr = inner.as_mut_ptr();

            for _ in 1..columns {
                ptr::write(ptr, T::default());
                ptr = ptr.offset(1);
            }
            ptr::write(ptr, T::default());

            inner.set_len(columns);
        }

        Row {
            inner,
            occ: 0,
            packed: None,
            command_mark: None,
        }
    }

    /// Increase the number of columns in the row.
    #[inline]
    pub fn grow(&mut self, columns: usize) {
        if self.len() >= columns {
            return;
        }

        if let Some(packed) = &mut self.packed {
            packed.columns = columns;
            packed.expanded.take();
        } else {
            self.inner.resize_with(columns, T::default);
        }
    }

    /// Reduce the number of columns in the row.
    ///
    /// This will return all non-empty cells that were removed.
    pub fn shrink(&mut self, columns: usize) -> Option<Vec<T>>
    where
        T: GridCell,
    {
        if self.len() <= columns {
            return None;
        }
        self.occ = min(self.occ, columns);

        if let Some(packed) = &mut self.packed {
            packed.expanded.take();
            packed.columns = columns;
            let suffix = packed.bytes.get(columns..).unwrap_or_default();
            let removed = suffix
                .iter()
                .rposition(|&byte| byte != b' ' && byte != b'\t')
                .map(|last| (packed.codec.expand)(&suffix[..=last], last + 1));
            packed.bytes.truncate(columns);
            return removed;
        }

        // Split off cells for a new row.
        let mut new_row = self.inner.split_off(columns);
        let index = new_row
            .iter()
            .rposition(|c| !c.is_empty())
            .map_or(0, |i| i + 1);
        new_row.truncate(index);

        if new_row.is_empty() {
            None
        } else {
            Some(new_row)
        }
    }

    /// Reset all cells in the row to the `template` cell.
    #[inline]
    pub fn reset<D>(&mut self, template: &T)
    where
        T: ResetDiscriminant<D> + GridCell,
        D: PartialEq,
    {
        self.reset_reusing(template, None);
    }

    pub(super) fn reset_reusing<D>(&mut self, template: &T, buffer: Option<(Vec<T>, usize)>)
    where
        T: ResetDiscriminant<D> + GridCell,
        D: PartialEq,
    {
        self.command_mark = None;
        if let Some(packed) = self.packed.take() {
            (self.inner, self.occ) = buffer.unwrap_or_else(|| (Self::new(packed.columns).inner, 0));
        }
        debug_assert!(!self.inner.is_empty());

        // Mark all cells as dirty if template cell changed.
        let len = self.inner.len();
        if self.inner[len - 1].discriminant() != template.discriminant() {
            self.occ = len;
        }

        // Reset every dirty cell in the row.
        for item in &mut self.inner[0..self.occ] {
            item.reset(template);
        }

        self.occ = 0;
    }
}

#[allow(clippy::len_without_is_empty)]
impl<T> Row<T> {
    #[inline]
    pub fn from_vec(vec: Vec<T>, occ: usize) -> Row<T> {
        Row {
            command_mark: None,
            inner: vec,
            occ,
            packed: None,
        }
    }

    pub(super) fn pack_ascii(&mut self) -> Option<(Vec<T>, usize)>
    where
        T: GridCell,
    {
        const SPACE: u8 = b' ';
        let codec = T::ascii_codec()?;
        if let Some(packed) = &mut self.packed {
            packed.expanded.take();
            return None;
        }
        let columns = self.inner.len();
        // The tail after occ is uniform. Omit it when it is the default blank cell.
        let occupied = if self.inner.last()?.ascii_byte() == Some(SPACE) {
            self.occ.min(columns)
        } else {
            columns
        };
        let mut bytes = Vec::with_capacity(occupied);
        for cell in &self.inner[..occupied] {
            bytes.push(cell.ascii_byte()?);
        }
        if let Some(last) = bytes.iter().rposition(|&cell| cell != SPACE) {
            bytes.truncate(last + 1);
        } else {
            bytes = Vec::new();
        }
        self.packed = Some(PackedRow {
            bytes,
            columns,
            codec,
            expanded: OnceLock::new(),
        });
        Some((mem::take(&mut self.inner), self.occ))
    }

    fn cells(&self) -> &[T] {
        match &self.packed {
            Some(packed) => packed
                .expanded
                .get_or_init(|| (packed.codec.expand)(&packed.bytes, packed.columns)),
            None => &self.inner,
        }
    }

    pub(super) fn cells_mut(&mut self) -> &mut Vec<T> {
        if let Some(packed) = self.packed.take() {
            self.inner = packed
                .expanded
                .into_inner()
                .unwrap_or_else(|| (packed.codec.expand)(&packed.bytes, packed.columns));
        }
        &mut self.inner
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.packed
            .as_ref()
            .map_or(self.inner.len(), |packed| packed.columns)
    }

    #[inline]
    pub fn last(&self) -> Option<&T> {
        self.len()
            .checked_sub(1)
            .map(|column| &self[Column(column)])
    }

    #[inline]
    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.occ = self.len();
        self.cells_mut().last_mut()
    }

    #[inline]
    pub fn append(&mut self, vec: &mut Vec<T>)
    where
        T: GridCell,
    {
        self.occ = self.len() + vec.len();
        self.cells_mut().append(vec);
    }

    #[inline]
    pub fn append_front(&mut self, mut vec: Vec<T>) {
        self.occ += vec.len();

        vec.append(self.cells_mut());
        self.inner = vec;
    }

    /// Check if all cells in the row are empty.
    #[inline]
    pub fn is_clear(&self) -> bool
    where
        T: GridCell,
    {
        (0..self.len()).all(|column| self[Column(column)].is_empty())
    }

    #[inline]
    pub fn front_split_off(&mut self, at: usize) -> Vec<T> {
        self.occ = self.occ.saturating_sub(at);

        let mut split = self.cells_mut().split_off(at);
        std::mem::swap(&mut split, &mut self.inner);
        split
    }
}

impl<'a, T> IntoIterator for &'a Row<T> {
    type IntoIter = slice::Iter<'a, T>;
    type Item = &'a T;

    #[inline]
    fn into_iter(self) -> slice::Iter<'a, T> {
        self.cells().iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Row<T> {
    type IntoIter = slice::IterMut<'a, T>;
    type Item = &'a mut T;

    #[inline]
    fn into_iter(self) -> slice::IterMut<'a, T> {
        self.occ = self.len();
        self.cells_mut().iter_mut()
    }
}

impl<T> Index<Column> for Row<T> {
    type Output = T;

    #[inline]
    fn index(&self, index: Column) -> &T {
        match &self.packed {
            Some(packed) => {
                assert!(index.0 < packed.columns);
                (packed.codec.get)(&packed.bytes, index.0)
            }
            None => &self.inner[index.0],
        }
    }
}

impl<T> IndexMut<Column> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Column) -> &mut T {
        self.occ = max(self.occ, *index + 1);
        &mut self.cells_mut()[index.0]
    }
}

impl<T> Index<Range<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: Range<Column>) -> &[T] {
        &self.cells()[(index.start.0)..(index.end.0)]
    }
}

impl<T> IndexMut<Range<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Range<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end);
        &mut self.cells_mut()[(index.start.0)..(index.end.0)]
    }
}

impl<T> Index<RangeTo<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeTo<Column>) -> &[T] {
        &self.cells()[..(index.end.0)]
    }
}

impl<T> IndexMut<RangeTo<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeTo<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end);
        &mut self.cells_mut()[..(index.end.0)]
    }
}

impl<T> Index<RangeFrom<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeFrom<Column>) -> &[T] {
        &self.cells()[(index.start.0)..]
    }
}

impl<T> IndexMut<RangeFrom<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeFrom<Column>) -> &mut [T] {
        self.occ = self.len();
        &mut self.cells_mut()[(index.start.0)..]
    }
}

impl<T> Index<RangeFull> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, _: RangeFull) -> &[T] {
        &self.cells()[..]
    }
}

impl<T> IndexMut<RangeFull> for Row<T> {
    #[inline]
    fn index_mut(&mut self, _: RangeFull) -> &mut [T] {
        self.occ = self.len();
        &mut self.cells_mut()[..]
    }
}

impl<T> Index<RangeToInclusive<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeToInclusive<Column>) -> &[T] {
        &self.cells()[..=(index.end.0)]
    }
}

impl<T> IndexMut<RangeToInclusive<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeToInclusive<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end + 1);
        &mut self.cells_mut()[..=(index.end.0)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::cell::{Cell, Flags};
    use crate::vte::ansi::{Color, NamedColor};

    #[test]
    fn packed_ascii_preserves_soft_wrap_and_reflow_cells() {
        let mut row = Row::<Cell>::new(8);
        row[Column(0)].c = 'x';
        row[Column(7)].flags.insert(Flags::WRAPLINE);
        let dense = row.clone();
        let buffer = row.pack_ascii().unwrap();
        assert_eq!(row[Column(0)], dense[Column(0)]);
        assert_eq!(row[Column(6)], Cell::default());
        assert_eq!(row[Column(7)], dense[Column(7)]);
        let wrapped = row.shrink(4).unwrap();
        assert!(wrapped.last().unwrap().flags.contains(Flags::WRAPLINE));
        row.grow(8);
        assert_eq!(row[Column(0)].c, 'x');
        assert_eq!(row[Column(7)], Cell::default());

        let background = Color::Named(NamedColor::Red);
        row.reset_reusing(&Cell::from(background), Some(buffer));
        assert!(
            row[..]
                .iter()
                .all(|cell| cell.c == ' ' && cell.bg == background)
        );
    }

    #[test]
    fn styled_and_unicode_rows_keep_full_cells() {
        let mut row = Row::<Cell>::new(4);
        row[Column(1)].c = '界';
        assert!(row.pack_ascii().is_none());
        row[Column(1)].c = 'a';
        row[Column(1)].fg = Color::Named(NamedColor::Red);
        assert!(row.pack_ascii().is_none());
        assert_eq!(row[Column(1)].fg, Color::Named(NamedColor::Red));
    }
}
