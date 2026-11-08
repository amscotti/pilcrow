//! Find-bar state. The query and replacement are transient view state, never
//! document bytes, history steps or autosave payloads.

use std::ops::Range;

/// Which find-bar field owns typed characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindField {
    Query,
    Replacement,
}

/// The in-editor find bar. The query is not a document: it disappears when the
/// bar closes and never enters history or autosave.
#[derive(Clone, Debug)]
pub(crate) struct FindState {
    pub(crate) query: String,
    pub(crate) replacement: String,
    pub(crate) field: FindField,
    pub(crate) query_cursor: usize,
    pub(crate) replacement_cursor: usize,
    pub(crate) matches: Vec<Range<usize>>,
    pub(crate) active: Option<usize>,
    /// Typed characters go to the bar. Clicking the page clears this while
    /// leaving the bar open.
    pub(crate) editing: bool,
    pub(crate) case_sensitive: bool,
}

impl FindState {
    pub(crate) fn cursor(&self) -> usize {
        match self.field {
            FindField::Query => self.query_cursor,
            FindField::Replacement => self.replacement_cursor,
        }
    }

    pub(crate) fn cursor_mut(&mut self) -> &mut usize {
        match self.field {
            FindField::Query => &mut self.query_cursor,
            FindField::Replacement => &mut self.replacement_cursor,
        }
    }

    pub(crate) fn value(&self) -> &str {
        match self.field {
            FindField::Query => &self.query,
            FindField::Replacement => &self.replacement,
        }
    }

    pub(crate) fn value_mut(&mut self) -> &mut String {
        match self.field {
            FindField::Query => &mut self.query,
            FindField::Replacement => &mut self.replacement,
        }
    }

    pub(crate) fn new(
        query: String,
        replacement: String,
        field: FindField,
        case_sensitive: bool,
    ) -> Self {
        let query_cursor = query.len();
        let replacement_cursor = replacement.len();
        Self {
            query,
            replacement,
            field,
            query_cursor,
            replacement_cursor,
            matches: Vec::new(),
            active: None,
            editing: true,
            case_sensitive,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_field_keeps_its_own_cursor() {
        let mut find = FindState::new("cat".into(), "dog".into(), FindField::Query, false);
        assert_eq!(find.cursor(), 3);
        find.value_mut().push('s');
        *find.cursor_mut() = 4;
        find.field = FindField::Replacement;
        assert_eq!(find.value(), "dog");
        assert_eq!(find.cursor(), 3);
        find.field = FindField::Query;
        assert_eq!(find.value(), "cats");
        assert_eq!(find.cursor(), 4);
    }
}
