//! Evaluates TOML configuration against module schemas.
//!
//! Configuration files are loaded from a [`ConfigFs`], checked against a [`Schema`] and merged
//! into one value per option. String values can reference other options and machine facts with
//! `${path}`. Every value keeps the file and line that set it, for error messages and `why`.

mod diag;
mod eval;
mod fs;
mod load;
mod module;
mod ordered;
mod path;
mod schema;
mod source;
mod suggest;
mod template;
mod value;
mod walk;
mod why;

pub use diag::{Diagnostic, Label, render};
pub use eval::{Contribution, EvalError, Evaluation, Input, Rendered, Resolved, Use, evaluate};
pub use fs::{ConfigFs, DirFs, MemFs};
pub use module::{
    ConfigReader, File, Module, ModuleId, Output, ReadError, RenderError, Section, Setting, Unit,
};
pub use path::OptionPath;
pub use schema::{ListMerge, OptionSpec, Record, Schema, Type};
pub use source::{FileId, Position, Sources, Span};
pub use template::Part;
pub use value::{RuntimePart, Value};
pub use walk::{Action, Definition, Level, Raw, Site};
pub use why::explain;

#[cfg(test)]
mod tests;
