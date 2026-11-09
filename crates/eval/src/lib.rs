//! Evaluates TOML configuration against module schemas.
//!
//! Configuration files are loaded from a [`ConfigFs`], checked against a [`Schema`] and merged
//! into one value per option. String values can reference other options and machine facts with
//! `${path}`. Every value keeps the file and line that set it, for error messages and `why`.

mod diag;
mod eval;
pub mod formats;
mod fs;
mod load;
mod machines;
mod module;
mod ordered;
mod path;
mod requires;
mod schema;
mod source;
mod suggest;
mod template;
mod value;
mod walk;
mod why;
mod writers;

pub use diag::{Diagnostic, Label, render};
pub use eval::{
    Contribution, EvalError, Evaluation, Input, MACHINE_VALUES, Rendered, Resolved, Use, evaluate,
};
pub use fs::{ConfigFs, DirFs, MemFs};
pub use load::{ConfigKind, DeclaredSource, kind_of, roles};
pub use machines::Machine;
pub use module::{
    Build, ConfigReader, DEFAULT_SOURCE, DISTRO_SOURCE, File, HOME_SECRETS_DIR, Home, Module,
    ModuleId, Output, ReadError, RenderError, SECRETS_DIR, Secret, Section, Setting, Template,
    Unit, UnitAction, Wrapper, home_path, package_source,
};
pub use path::OptionPath;
pub use requires::{Check, FACTS, Kind, Requirement};
pub use schema::{EVERY, ListMerge, OptionSpec, Record, Schema, Type};
pub use source::{FileId, Position, Sources, Span};
pub use template::Part;
pub use value::{RuntimePart, Value};
pub use walk::{Action, Definition, Level, Raw, Site};
pub use why::{describe_outputs, explain, explain_output, origin, outputs_of};

#[cfg(test)]
mod tests;
