//! Names used in type annotations.
//!
//! Annotations are parsed but not checked at run time, so a module can also be valid,
//! type-checked Python. The compiler still resolves every name in them, though, and names such
//! as `Ctx` or a generated `ServicesSyncthing` only exist in the Python stubs. The loader binds
//! those names to `None` before evaluating a module.

use std::collections::BTreeSet;

use starlark::syntax::AstModule;
use starlark_syntax::syntax::ast::{AstExpr, AstStmt, Expr, ParameterP, Stmt};
use starlark_syntax::syntax::uniplate::Visit;

/// Every identifier that appears inside a parameter or return type annotation.
pub(crate) fn names(ast: &AstModule) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    statement(ast.statement(), &mut names);
    names
}

fn statement(stmt: &AstStmt, names: &mut BTreeSet<String>) {
    if let Stmt::Def(def) = &stmt.node {
        for param in &def.params {
            match &param.node {
                ParameterP::Normal(_, Some(ty), _)
                | ParameterP::Args(_, Some(ty))
                | ParameterP::KwArgs(_, Some(ty)) => expression(&ty.node.expr, names),
                ParameterP::Normal(..)
                | ParameterP::Args(..)
                | ParameterP::KwArgs(..)
                | ParameterP::Slash
                | ParameterP::NoArgs => {}
            }
        }
        if let Some(ty) = &def.return_type {
            expression(&ty.node.expr, names);
        }
    }
    stmt.visit_children(|child| match child {
        Visit::Stmt(child) => statement(child, names),
        Visit::Expr(_) => {}
    });
}

fn expression(expr: &AstExpr, names: &mut BTreeSet<String>) {
    if let Expr::Identifier(ident) = &expr.node {
        names.insert(ident.node.ident.clone());
    }
    expr.visit_expr(|child| expression(child, names));
}
