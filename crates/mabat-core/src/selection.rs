//! The fields of a view to load, such as a GraphQL selection set.
//!
//! A [`Selection`] names fields of a view, and for collections and references the fields of
//! their view. A view selected without fields, such as a collection selected by its name
//! alone, loads its columns and embedded values but none of its collections or references,
//! so that every selection has a finite depth. Embedded structs and enums are loaded whole.
//! The plan of a selection only selects the columns and runs the child queries the selected
//! fields need.

use std::fmt;

/// Selected fields, each with the selection of its own fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    fields: Vec<(String, Selection)>,
}

/// Why a selection could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid selection at byte {position}: {message}")]
pub struct SelectionError {
    pub position: usize,
    pub message: String,
}

impl Selection {
    /// No fields selected yet.
    pub fn new() -> Selection {
        Selection::default()
    }

    /// Select a field. A collection or reference selected this way loads the columns and
    /// embedded values of its view.
    pub fn field(self, name: impl Into<String>) -> Selection {
        self.nested(name, Selection::new())
    }

    /// Select a collection or a reference with the given fields of its view.
    pub fn nested(mut self, name: impl Into<String>, selection: Selection) -> Selection {
        let name = name.into();
        match self.fields.iter_mut().find(|(n, _)| *n == name) {
            Some((_, existing)) => existing.merge(selection),
            None => self.fields.push((name, selection)),
        }
        self
    }

    /// Add the fields of `other`, as GraphQL merges the selections of a field.
    pub fn merge(&mut self, other: Selection) {
        for (name, selection) in other.fields {
            match self.fields.iter_mut().find(|(n, _)| *n == name) {
                Some((_, existing)) => existing.merge(selection),
                None => self.fields.push((name, selection)),
            }
        }
    }

    /// The selection of a field, if it is selected.
    pub fn get(&self, name: &str) -> Option<&Selection> {
        self.fields.iter().find(|(n, _)| n == name).map(|(_, s)| s)
    }

    /// `true` if no field is selected: for a view, its columns and embedded values.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// The selected fields, in order.
    pub fn fields(&self) -> impl Iterator<Item = (&str, &Selection)> {
        self.fields.iter().map(|(n, s)| (n.as_str(), s))
    }

    /// Parse a selection written like a GraphQL selection set, with or without the outer
    /// braces: `name assignee { name } children { name position }`. Commas are whitespace.
    pub fn parse(text: &str) -> Result<Selection, SelectionError> {
        let mut parser = Parser { text, position: 0 };
        parser.skip();
        let selection = if parser.peek() == Some('{') {
            parser.position += 1;
            let selection = parser.fields(true)?;
            parser.skip();
            selection
        } else {
            parser.fields(false)?
        };
        match parser.peek() {
            None => Ok(selection),
            Some(c) => Err(parser.error(format!("unexpected `{c}`"))),
        }
    }
}

impl fmt::Display for Selection {
    /// The selection in the syntax of [`Selection::parse`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (name, selection)) in self.fields.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            f.write_str(name)?;
            if !selection.is_empty() {
                write!(f, " {{ {selection} }}")?;
            }
        }
        Ok(())
    }
}

struct Parser<'a> {
    text: &'a str,
    position: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.position..].chars().next()
    }

    fn skip(&mut self) {
        while let Some(c) = self.peek() {
            if !(c.is_whitespace() || c == ',') {
                break;
            }
            self.position += c.len_utf8();
        }
    }

    fn error(&self, message: String) -> SelectionError {
        SelectionError { position: self.position, message }
    }

    /// Fields up to the end, or up to `}` if `braced`, which is consumed.
    fn fields(&mut self, braced: bool) -> Result<Selection, SelectionError> {
        let mut selection = Selection::new();
        loop {
            self.skip();
            match self.peek() {
                None if braced => return Err(self.error("missing `}`".into())),
                None => return Ok(selection),
                Some('}') if braced => {
                    self.position += 1;
                    if selection.is_empty() {
                        return Err(self.error("empty `{ }`".into()));
                    }
                    return Ok(selection);
                }
                Some(c) if c.is_alphabetic() || c == '_' => {
                    let start = self.position;
                    while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        self.position += self.peek().map_or(0, char::len_utf8);
                    }
                    let name = &self.text[start..self.position];
                    self.skip();
                    let nested = if self.peek() == Some('{') {
                        self.position += 1;
                        self.fields(true)?
                    } else {
                        Selection::new()
                    };
                    selection = selection.nested(name, nested);
                }
                Some(c) => return Err(self.error(format!("unexpected `{c}`"))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_fields() {
        let selection = Selection::parse("{ name, assignee { name } children { name position } }").unwrap();
        assert_eq!(
            selection,
            Selection::new()
                .field("name")
                .nested("assignee", Selection::new().field("name"))
                .nested("children", Selection::new().field("name").field("position"))
        );
        assert_eq!(selection.to_string(), "name assignee { name } children { name position }");
        assert_eq!(Selection::parse(&selection.to_string()).unwrap(), selection);
    }

    #[test]
    fn merges_repeated_fields() {
        let selection = Selection::parse("a { x } b a { y }").unwrap();
        assert_eq!(selection.to_string(), "a { x y } b");
    }

    #[test]
    fn reports_errors_with_their_position() {
        assert_eq!(Selection::parse("a { b").unwrap_err().message, "missing `}`");
        assert_eq!(Selection::parse("a { }").unwrap_err().position, 5);
        assert_eq!(Selection::parse("a } b").unwrap_err().message, "unexpected `}`");
        assert_eq!(Selection::parse("a-b").unwrap_err().position, 1);
        assert!(Selection::parse("").unwrap().is_empty());
    }
}
