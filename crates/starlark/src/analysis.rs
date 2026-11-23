//! What a module reads and writes, from its code (0044).
//!
//! The context is always annotated, `ctx: Context`, so the calls on it are found from the syntax
//! alone: every `.set`, `.default`, `.remove` and `.read` on a parameter of that type, in a
//! module or in a library it loads. What a call reaches is its path argument, as far as it's
//! written out: a literal, or the literal start of `"a.b." + name` or `"a.%s.c" % name`, cut back
//! to its last whole segment.
//!
//! A value read from a literal path is followed while it's indexed with literal keys, as in
//! `ctx.read("services.ssh")["port"]`, `cfg["port"]` for a `cfg` assigned once from such a read, or
//! `cfg.get("port", 22)`. Each key is a field, checked against the option's declaration.

use std::collections::{HashMap, HashSet};

use starlark::codemap::{FileSpan, Span};
use starlark::syntax::AstModule;
use starlark::syntax::ast::{
    ArgumentP, AssignTargetP, AstExpr, AstLiteral, AstNoPayload, AstParameter, AstStmt, BinOp,
    CallArgsP, ClauseP, DefP, ExprP, ParameterP, StmtP,
};
use starlark_syntax::syntax::uniplate::Visit;
use wrath_eval::OptionPath;

const WRITES: [&str; 3] = ["set", "default", "remove"];
const READ: &str = "read";

/// A call that reads or writes an option, and where it is.
#[derive(Debug, Clone)]
pub(crate) struct Access {
    pub path: OptionPath,
    pub at: FileSpan,
}

/// What a file's calls on the context reach.
#[derive(Debug, Clone, Default)]
pub(crate) struct Accesses {
    pub writes: Vec<Access>,
    pub reads: Vec<Access>,
    /// Fields of values read, as the read's path followed by the keys.
    pub fields: Vec<Access>,
}

/// Something the analysis can't follow, refused where it's written.
#[derive(Debug)]
pub(crate) struct Refused {
    pub at: FileSpan,
    pub message: String,
}

/// The options `ast`'s calls on the context read and write.
pub(crate) fn accesses(ast: &AstModule) -> Result<Accesses, Refused> {
    let mut walk = Walk {
        ast,
        context: Vec::new(),
        bound: HashMap::new(),
        once: HashSet::new(),
        found: Accesses::default(),
        refused: None,
    };
    walk.stmt(ast.statement());
    match walk.refused {
        Some(refused) => Err(refused),
        None => Ok(walk.found),
    }
}

struct Walk<'a> {
    ast: &'a AstModule,
    /// The names of the context parameters in scope.
    context: Vec<String>,
    /// Names holding a value read from an option, by the option's path.
    bound: HashMap<String, OptionPath>,
    /// Names the enclosing function binds once, so what they hold can be followed.
    once: HashSet<String>,
    found: Accesses,
    refused: Option<Refused>,
}

impl Walk<'_> {
    fn refuse(&mut self, span: Span, message: String) {
        if self.refused.is_none() {
            self.refused = Some(Refused {
                at: self.ast.file_span(span),
                message,
            });
        }
    }

    fn stmt(&mut self, stmt: &AstStmt) {
        match &stmt.node {
            StmtP::Def(def) => self.def(def),
            StmtP::Assign(assign) => {
                stmt.visit_children(|child| self.visit(child));
                if let AssignTargetP::Identifier(name) = &assign.lhs.node
                    && self.once.contains(&name.node.ident)
                    && let Some(path) = self.value_path(&assign.rhs)
                {
                    self.bound.insert(name.node.ident.clone(), path);
                }
            }
            _ => stmt.visit_children(|child| self.visit(child)),
        }
    }

    fn def(&mut self, def: &DefP<AstNoPayload>) {
        let counts = binders(def);
        let outer_bound = self.bound.clone();
        self.bound.retain(|name, _| !counts.contains_key(name));
        let outer_once = std::mem::replace(
            &mut self.once,
            counts
                .into_iter()
                .filter_map(|(name, count)| (count == 1).then_some(name))
                .collect(),
        );
        let outer = self.context.len();
        for param in &def.params {
            if let ParameterP::Normal(name, ty, default) = &param.node {
                let annotated = ty.as_ref().map(|ty| &ty.expr.node);
                match annotated {
                    Some(ExprP::Identifier(ty)) if ty.node.ident == "Context" => {
                        self.context.push(name.node.ident.clone());
                    }
                    None if name.node.ident == "ctx" => self.refuse(
                        param.span,
                        "the context needs its type, so what it's used for can be told: \
                         `ctx: Context`"
                            .to_owned(),
                    ),
                    _ => {}
                }
                if let Some(default) = default {
                    self.expr(default);
                }
            }
        }
        self.stmt(&def.body);
        self.context.truncate(outer);
        self.bound = outer_bound;
        self.once = outer_once;
    }

    /// The option a value comes from, when it's read from a literal path and indexed only with
    /// literal keys.
    fn value_path(&self, expr: &AstExpr) -> Option<OptionPath> {
        match &expr.node {
            ExprP::Identifier(name) => self.bound.get(&name.node.ident).cloned(),
            ExprP::Index(pair) => Some(self.value_path(&pair.0)?.child(literal(&pair.1)?)),
            ExprP::Call(callee, args) => {
                let key = args.args.first().and_then(|arg| match &arg.node {
                    ArgumentP::Positional(key) => literal(key),
                    _ => None,
                })?;
                if self.context_method(callee) == Some(READ) {
                    return OptionPath::parse(key).ok();
                }
                match &callee.node {
                    ExprP::Dot(on, method) if method.node == "get" => {
                        Some(self.value_path(on)?.child(key))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Records `expr` as a field when it's one of a value read from an option.
    fn field(&mut self, expr: &AstExpr) {
        let is_key = match &expr.node {
            ExprP::Index(..) => true,
            ExprP::Call(callee, _) => {
                matches!(&callee.node, ExprP::Dot(_, method) if method.node == "get")
            }
            _ => false,
        };
        if is_key && let Some(path) = self.value_path(expr) {
            let at = self.ast.file_span(expr.span);
            self.found.fields.push(Access { path, at });
        }
    }

    fn visit(&mut self, child: Visit<'_, AstNoPayload>) {
        match child {
            Visit::Stmt(stmt) => self.stmt(stmt),
            Visit::Expr(expr) => self.expr(expr),
        }
    }

    /// The method `expr` names on a context parameter, as in `ctx.set`.
    fn context_method<'e>(&self, expr: &'e AstExpr) -> Option<&'e str> {
        let ExprP::Dot(on, method) = &expr.node else {
            return None;
        };
        let ExprP::Identifier(name) = &on.node else {
            return None;
        };
        self.context
            .contains(&name.node.ident)
            .then_some(method.node.as_str())
    }

    fn is_context(&self, expr: &AstExpr) -> bool {
        matches!(&expr.node, ExprP::Identifier(name) if self.context.contains(&name.node.ident))
    }

    fn expr(&mut self, expr: &AstExpr) {
        self.field(expr);
        match &expr.node {
            ExprP::Call(callee, args) => {
                if let Some(method) = self.context_method(callee) {
                    if is_access(method) {
                        self.call(method, args, expr.span);
                    }
                    args.args.iter().for_each(|arg| self.expr(arg.node.expr()));
                    return;
                }
                if let ExprP::Identifier(name) = &callee.node
                    && name.node.ident == "getattr"
                    && args
                        .args
                        .first()
                        .is_some_and(|arg| self.is_context(arg.node.expr()))
                {
                    self.refuse(
                        expr.span,
                        "`getattr` on the context hides what it reads and writes: call the \
                         method by name"
                            .to_owned(),
                    );
                }
            }
            ExprP::Dot(..) => {
                if let Some(method) = self.context_method(expr)
                    && is_access(method)
                {
                    self.refuse(
                        expr.span,
                        format!(
                            "`ctx.{method}` taken as a value hides what it reaches: call it \
                             where it's used"
                        ),
                    );
                }
            }
            _ => {}
        }
        expr.visit_expr(|child| self.expr(child));
    }

    fn call(&mut self, method: &str, args: &CallArgsP<AstNoPayload>, span: Span) {
        let Some(ArgumentP::Positional(path)) = args.args.first().map(|arg| &arg.node) else {
            return;
        };
        let reached = match start(path) {
            Start::Whole(text) => Some(text),
            Start::Prefix(text) => whole_segments(&text),
            Start::None => None,
        };
        let at = self.ast.file_span(span);
        match (reached, method == READ) {
            (Some(text), _) => match OptionPath::parse(&text) {
                Ok(path) => {
                    let access = Access { path, at };
                    if method == READ {
                        self.found.reads.push(access);
                    } else {
                        self.found.writes.push(access);
                    }
                }
                Err(e) => self.refuse(span, e),
            },
            // A read whose path is computed is still a read, just not one that orders anything.
            (None, true) => {}
            (None, false) => self.refuse(
                span,
                "what this writes can't be told, as its path starts computed: write the \
                 option's path where it's set, such as `\"security.pam.services.\" + name`"
                    .to_owned(),
            ),
        }
    }
}

fn is_access(method: &str) -> bool {
    WRITES.contains(&method) || method == READ
}

fn literal(expr: &AstExpr) -> Option<&str> {
    match &expr.node {
        ExprP::Literal(AstLiteral::String(text)) => Some(&text.node),
        _ => None,
    }
}

/// Every name `def` binds anywhere inside it, nested functions and comprehensions included, with
/// how many times.
fn binders(def: &DefP<AstNoPayload>) -> HashMap<String, usize> {
    fn name(counts: &mut HashMap<String, usize>, name: &str) {
        *counts.entry(name.to_owned()).or_default() += 1;
    }
    fn target(counts: &mut HashMap<String, usize>, target: &AssignTargetP<AstNoPayload>) {
        target.visit_lvalue(|ident| name(counts, &ident.ident));
    }
    fn params(counts: &mut HashMap<String, usize>, params: &[AstParameter]) {
        for param in params {
            if let Some(ident) = param.node.ident() {
                name(counts, &ident.ident);
            }
        }
    }
    fn stmt(counts: &mut HashMap<String, usize>, s: &AstStmt) {
        match &s.node {
            StmtP::Assign(assign) => target(counts, &assign.lhs),
            StmtP::AssignModify(lhs, ..) => target(counts, lhs),
            StmtP::For(each) => target(counts, &each.var),
            StmtP::Def(def) => {
                name(counts, &def.name.ident);
                params(counts, &def.params);
            }
            _ => {}
        }
        s.visit_children(|child| match child {
            Visit::Stmt(s) => stmt(counts, s),
            Visit::Expr(e) => expr(counts, e),
        });
    }
    fn expr(counts: &mut HashMap<String, usize>, e: &AstExpr) {
        match &e.node {
            ExprP::ListComprehension(_, first, rest) | ExprP::DictComprehension(_, first, rest) => {
                target(counts, &first.var);
                for clause in rest {
                    if let ClauseP::For(each) = clause {
                        target(counts, &each.var);
                    }
                }
            }
            ExprP::Lambda(lambda) => params(counts, &lambda.params),
            _ => {}
        }
        e.visit_expr(|child| expr(counts, child));
    }
    let mut counts = HashMap::new();
    params(&mut counts, &def.params);
    stmt(&mut counts, &def.body);
    counts
}

/// How much of a path expression is written out.
enum Start {
    Whole(String),
    Prefix(String),
    None,
}

fn start(expr: &AstExpr) -> Start {
    match &expr.node {
        ExprP::Literal(AstLiteral::String(text)) => Start::Whole(text.node.clone()),
        ExprP::Op(left, BinOp::Add, right) => match start(left) {
            Start::Whole(head) => match start(right) {
                Start::Whole(tail) => Start::Whole(head + &tail),
                Start::Prefix(tail) => Start::Prefix(head + &tail),
                Start::None => Start::Prefix(head),
            },
            prefix => prefix,
        },
        ExprP::Op(format, BinOp::Percent, _) => match &format.node {
            ExprP::Literal(AstLiteral::String(text)) => {
                let head = text.node.split('%').next().unwrap_or_default();
                Start::Prefix(head.to_owned())
            }
            _ => Start::None,
        },
        _ => Start::None,
    }
}

/// `text` up to its last whole segment: `a.b.` and `a.b.c` without its end both give `a.b`.
fn whole_segments(text: &str) -> Option<String> {
    let cut = text.rfind('.')?;
    Some(text[..cut].to_owned()).filter(|path| !path.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyse(source: &str) -> Result<Accesses, String> {
        let ast = crate::parse("m.star", source).map_err(|e| e.to_string())?;
        accesses(&ast).map_err(|refused| refused.message)
    }

    fn paths(accesses: &[Access]) -> Vec<String> {
        accesses
            .iter()
            .map(|access| access.path.to_string())
            .collect()
    }

    #[test]
    fn literal_and_partly_computed_paths_are_found() {
        let found = analyse(
            r#"
def render(ctx: Context) -> None:
    ctx.set("networking.firewall.allowed_tcp_ports", [22])
    for service in ["login", "sudo"]:
        ctx.set("security.pam.services.%s.auth" % service, [])
        ctx.default("users." + service, {})
    if ctx.read("services.ssh.enable"):
        ctx.remove("boot.kernel_params", ["quiet"])
"#,
        )
        .unwrap();
        assert_eq!(
            paths(&found.writes),
            [
                "networking.firewall.allowed_tcp_ports",
                "security.pam.services",
                "users",
                "boot.kernel_params",
            ]
        );
        assert_eq!(paths(&found.reads), ["services.ssh.enable"]);
    }

    #[test]
    fn helpers_that_take_the_context_are_followed_and_others_ignored() {
        let found = analyse(
            r#"
def open(ctx: Context, port: int) -> None:
    ctx.set("networking.firewall.allowed_tcp_ports", [port])

def unrelated(settings: dict[str, typing.Any]) -> None:
    settings.set("not.an.option", 1)

def render(ctx: Context) -> None:
    open(ctx, 22)
"#,
        )
        .unwrap();
        assert_eq!(
            paths(&found.writes),
            ["networking.firewall.allowed_tcp_ports"]
        );
    }

    #[test]
    fn a_computed_read_is_allowed_and_orders_nothing() {
        let found = analyse(
            r#"
def render(ctx: Context) -> None:
    name = "ssh"
    ctx.read(name)
    ctx.read("users." + name)
"#,
        )
        .unwrap();
        assert_eq!(paths(&found.reads), ["users"]);
    }

    #[test]
    fn literal_keys_on_values_read_are_fields() {
        let found = analyse(
            r#"
def render(ctx: Context) -> None:
    cfg = ctx.read("services.ssh")
    ctx.set("networking.firewall.allowed_tcp_ports", [cfg["port"]])
    if cfg.get("settings", {})["x11"]:
        pass
    users = ctx.read("users")["john"]["groups"]
    ctx.read("boot").get(cfg["key"])
"#,
        )
        .unwrap();
        assert_eq!(
            paths(&found.fields),
            [
                "services.ssh.port",
                "services.ssh.settings.x11",
                "services.ssh.settings",
                "users.john.groups",
                "users.john",
                "services.ssh.key",
            ]
        );
    }

    #[test]
    fn names_bound_more_than_once_or_from_elsewhere_are_not_followed() {
        let found = analyse(
            r#"
def render(ctx: Context) -> None:
    cfg = ctx.read("services.ssh")
    if True:
        cfg = {"other": 1}
    cfg["other"]
    ssh = ctx.read("services.ssh")
    [ssh["x"] for ssh in [{"x": 1}]]
    name = "port"
    ctx.read("services.ssh")[name]

def helper(ctx: Context, cfg: dict[str, typing.Any]) -> None:
    cfg["anything"]
"#,
        )
        .unwrap();
        assert_eq!(paths(&found.fields), Vec::<String>::new());
    }

    #[test]
    fn what_cant_be_followed_is_refused() {
        let refused = [
            (
                "def r(ctx: Context) -> None:\n    ctx.set(path(), 1)\n",
                "starts computed",
            ),
            (
                "def r(ctx: Context) -> None:\n    ctx.set(\"%s.x\" % a, 1)\n",
                "starts computed",
            ),
            (
                "def r(ctx: Context) -> None:\n    f = ctx.set\n",
                "taken as a value",
            ),
            (
                "def r(ctx: Context) -> None:\n    getattr(ctx, \"set\")\n",
                "getattr",
            ),
            ("def r(ctx) -> None:\n    pass\n", "`ctx: Context`"),
        ];
        for (source, says) in refused {
            let message = analyse(source).unwrap_err();
            assert!(message.contains(says), "{source}: {message}");
        }
    }
}
