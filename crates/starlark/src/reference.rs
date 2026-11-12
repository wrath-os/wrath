//! The module API as Markdown, made from the doc comments of what modules can call, so the
//! reference can't drift from the code.

use std::fmt::Write;

use starlark::docs::{DocFunction, DocItem, DocMember, DocParam, DocType};
use starlark::environment::GlobalsBuilder;

use super::{Ctx, PLAIN_TYPES, formats_functions, std_globals, types_namespace};

/// Every function a module can call, the `types` and `formats` namespaces, and the methods of
/// the `ctx` its `render` gets.
pub fn module_api() -> String {
    let globals = GlobalsBuilder::new()
        .with(std_globals)
        .with_namespace("types", types_namespace)
        .with_namespace("formats", formats_functions)
        .build()
        .documentation();
    let mut out = String::new();
    let _ = writeln!(out, "## Functions\n");
    for (name, item) in &globals.members {
        if let DocItem::Member(DocMember::Function(function)) = item {
            describe(&mut out, name, function);
        }
    }
    for (namespace, item) in &globals.members {
        let DocItem::Module(module) = item else {
            continue;
        };
        let _ = writeln!(out, "## `{namespace}`\n");
        if namespace == "types" {
            let plain: Vec<String> = PLAIN_TYPES
                .iter()
                .map(|(name, _)| format!("`types.{name}`"))
                .collect();
            let _ = writeln!(out, "Types that take nothing: {}.\n", plain.join(", "));
        }
        for (name, item) in &module.members {
            if let DocItem::Member(DocMember::Function(function)) = item {
                describe(&mut out, &format!("{namespace}.{name}"), function);
            }
        }
    }
    let _ = writeln!(out, "## `ctx`\n");
    let _ = writeln!(
        out,
        "What a module's `render` is given, to read options and say what the system gets.\n"
    );
    for (name, member) in &DocType::from_starlark_value::<Ctx>().members {
        if let DocMember::Function(function) = member {
            describe(&mut out, &format!("ctx.{name}"), function);
        }
    }
    out
}

/// A heading with the call's signature, then its documentation.
fn describe(out: &mut String, name: &str, function: &DocFunction) {
    let params = &function.params;
    let mut parts: Vec<String> = params
        .pos_only
        .iter()
        .chain(&params.pos_or_named)
        .map(param)
        .collect();
    if let Some(args) = &params.args {
        parts.push(format!("*{}", args.name));
    } else if !params.named_only.is_empty() {
        parts.push("*".to_owned());
    }
    parts.extend(params.named_only.iter().map(param));
    if let Some(kwargs) = &params.kwargs {
        parts.push(format!("**{}", kwargs.name));
    }
    let _ = writeln!(out, "### `{name}({})`\n", parts.join(", "));
    if let Some(docs) = &function.docs {
        let _ = writeln!(out, "{}\n", docs.summary);
        if let Some(details) = &docs.details {
            let _ = writeln!(out, "{details}\n");
        }
    }
}

fn param(param: &DocParam) -> String {
    match &param.default_value {
        Some(default) => format!("{} = {default}", param.name),
        None => param.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_has_the_functions_namespaces_and_ctx_with_their_docs() {
        let text = module_api();
        assert!(text.contains("### `module("), "{text}");
        assert!(text.contains("### `types.list(item)`"), "{text}");
        assert!(text.contains("`types.str`"), "{text}");
        assert!(text.contains("### `formats.json(value)`"), "{text}");
        assert!(
            text.contains("### `ctx.read(path)`\n\nThe value of an option"),
            "{text}"
        );
    }
}
