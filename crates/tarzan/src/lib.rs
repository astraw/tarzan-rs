pub mod format;
mod io;
mod reader;
mod wrap;

pub use crate::reader::TarzanReader;
pub use crate::wrap::{WrapOptions, wrap};
