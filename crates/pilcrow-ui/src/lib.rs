//! Pilcrow design tokens, type scale and page geometry.
//!
//! The UI crate is deliberately small: it owns the numbers the page and the
//! chrome must agree on. It paints nothing and knows nothing about documents.

pub mod page;
pub mod theme;
pub mod typography;
