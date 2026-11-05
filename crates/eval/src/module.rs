//! The host API between the evaluator and modules, independent of the language modules are
//! written in.
//!
//! A module declares options, names the option prefixes it may set, and renders: it reads
//! options through a [`ConfigReader`] and returns settings for other options plus the files,
//! units and packages it contributes. A module cannot read anything under the prefixes it sets,
//! so contributions only ever flow one way.

use std::collections::{BTreeMap, BTreeSet};

use crate::diag::Diagnostic;
use crate::path::OptionPath;
use crate::schema::OptionSpec;
use crate::source::Span;
use crate::value::Value;
use crate::walk::Level;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(pub(crate) u32);

impl ModuleId {
    pub(crate) fn from_index(index: usize) -> Self {
        Self(u32::try_from(index).expect("fewer than 2^32 modules"))
    }

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

pub trait Module {
    fn name(&self) -> &str;

    /// Options the module declares, by absolute path.
    fn options(&self) -> Vec<(OptionPath, OptionSpec)>;

    /// Option prefixes the module may set.
    fn writes(&self) -> Vec<OptionPath>;

    fn render(&self, config: &mut dyn ConfigReader) -> Result<Output, RenderError>;
}

pub trait ConfigReader {
    /// The value of an option, or a table of everything set under a table of options. `None`
    /// when an option without a default isn't set. `at` is where the module reads it.
    fn read(&mut self, path: &OptionPath, at: Option<Span>) -> Result<Option<Value>, ReadError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The read itself is wrong, such as an unknown option. The module should fail with this.
    Invalid(String),
    /// The option failed to evaluate and the error is already reported. The module should stop
    /// with [`RenderError::Failed`].
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// Stopped because something it read failed; already reported.
    Failed,
    Error(Diagnostic),
}

/// What a module contributes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Output {
    pub settings: Vec<Setting>,
    pub packages: BTreeSet<String>,
    /// Files by absolute path.
    pub files: BTreeMap<String, File>,
    /// systemd units by name, such as `syncthing.service`.
    pub units: BTreeMap<String, Unit>,
}

/// A value a module sets for an option, or for a table of options.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub path: OptionPath,
    pub value: Value,
    pub level: Level,
    pub at: Option<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub text: String,
    pub mode: u32,
}

/// A unit file as ordered sections of ordered entries. Keys may repeat, as in `ExecStartPre`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unit {
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub entries: Vec<(String, String)>,
}
