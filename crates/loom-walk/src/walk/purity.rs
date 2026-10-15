//! Fail closed on raw analysis I/O, including swallowed reads and import aliases.
//!
//! This audits the Loom-owned rule/helper source, not arbitrary third-party Rust.
//! Only the explicit disk adapter and standalone root discovery may touch disk.

use loom_gate::inventory::snapshot::Snapshot;
use loom_gate::provider::Error;
use syn::visit::Visit;

pub(super) fn require(snapshot: &Snapshot) -> Result<(), Error> {
    for (path, bytes) in snapshot.files() {
        if !path.as_path().starts_with("crates/loom-walk/src")
            || path.as_path() == std::path::Path::new("crates/loom-walk/src/main.rs")
            || path.as_path().extension().is_none_or(|ext| ext != "rs")
        {
            continue;
        }
        let text =
            std::str::from_utf8(bytes).map_err(|_| Error::Encoding { path: path.clone() })?;
        let tree = syn::parse_file(text).map_err(|error| Error::UntrackedIo {
            checker: path.clone(),
            operation: format!("unauditable Rust: {error}"),
        })?;
        let mut audit = Audit {
            adapter: path.as_path().ends_with("walk/source.rs"),
            query: path.as_path().ends_with("walk/inputs.rs"),
            frontend: path.as_path().ends_with("walk/mod.rs"),
            operation: None,
        };
        audit.visit_file(&tree);
        if let Some(operation) = audit.operation {
            return Err(Error::UntrackedIo {
                checker: path.clone(),
                operation,
            });
        }
    }
    Ok(())
}

struct Audit {
    adapter: bool,
    query: bool,
    frontend: bool,
    operation: Option<String>,
}

fn test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Path>()
                .is_ok_and(|path| path.is_ident("test"))
    })
}

impl Audit {
    fn deny(&mut self, operation: impl Into<String>) {
        self.operation = Some(operation.into());
    }
    fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
        // Ignore literals (including source-code fixtures); inspect nested executable tokens.
        let mut previous = String::new();
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Ident(ident) => {
                    let ident = ident.to_string();
                    if matches!(
                        ident.as_str(),
                        "fs" | "walkdir" | "net" | "process" | "env" | "thread"
                    ) || matches!(previous.as_str(), "process") && ident == "Command"
                    {
                        self.deny(ident.clone());
                    }
                    previous = ident;
                }
                proc_macro2::TokenTree::Group(group) => self.tokens(group.stream()),
                _ => {}
            }
        }
    }
}

fn import_paths(tree: &syn::UseTree, prefix: &str, out: &mut Vec<String>) {
    match tree {
        syn::UseTree::Path(p) => import_paths(&p.tree, &format!("{prefix}{}::", p.ident), out),
        syn::UseTree::Group(g) => {
            for tree in &g.items {
                import_paths(tree, prefix, out);
            }
        }
        syn::UseTree::Name(n) => out.push(format!("{prefix}{}", n.ident)),
        syn::UseTree::Rename(n) => out.push(format!("{prefix}{}", n.ident)),
        syn::UseTree::Glob(_) => out.push(format!("{prefix}*")),
    }
}

impl<'ast> Visit<'ast> for Audit {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !test_only(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if test_only(&item.attrs)
            || self.adapter && item.sig.ident == "workspace_root"
            || self.frontend && item.sig.ident == "from_env"
            || self.query
                && matches!(
                    item.sig.ident.to_string().as_str(),
                    "inputs_for" | "render_print_inputs"
                )
        {
            return;
        }
        syn::visit::visit_item_fn(self, item);
    }
    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if test_only(&item.attrs)
            || self.frontend && matches!(item.sig.ident.to_string().as_str(), "from_env" | "reader")
        {
            return;
        }
        syn::visit::visit_impl_item_fn(self, item);
    }
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if self.adapter && matches!(&*item.self_ty, syn::Type::Path(ty) if ty.path.is_ident("Disk"))
        {
            return;
        }
        syn::visit::visit_item_impl(self, item);
    }
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if test_only(&item.attrs) {
            return;
        }
        // Use-tree traversal catches `use std::{fs as storage}` and glob aliases.
        let mut imports = Vec::new();
        import_paths(&item.tree, "", &mut imports);
        for import in imports {
            if import.starts_with("std::fs")
                || import.starts_with("tokio::fs")
                || import.starts_with("std::process")
                || import.starts_with("walkdir")
                || import.starts_with("std::env")
                || import.starts_with("std::net")
                || import.starts_with("tokio::net")
                || import.starts_with("std::thread")
                || import.starts_with("std::io::Read")
                || import.starts_with("tokio::io::AsyncRead")
                || matches!(
                    import.as_str(),
                    "std::*" | "std::io::*" | "tokio::*" | "tokio::io::*"
                )
            {
                self.deny(import);
            }
        }
        syn::visit::visit_item_use(self, item);
    }
    fn visit_path(&mut self, path: &'ast syn::Path) {
        let names: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if names.iter().any(|name| {
            matches!(
                name.as_str(),
                "fs" | "walkdir" | "net" | "process" | "env" | "thread"
            )
        }) || names
            .windows(2)
            .any(|pair| pair[0] == "process" && pair[1] == "Command")
        {
            self.deny(names.join("::"));
        }
        if !self.adapter
            && names.windows(2).any(|pair| {
                pair[0] == "WalkInput"
                    && matches!(
                        pair[1].as_str(),
                        "default" | "from_env" | "for_root" | "parse_files"
                    )
            })
        {
            self.deny(names.join("::"));
        }
        if names
            .windows(2)
            .any(|pair| pair[0] == "Default" && pair[1] == "default")
            || !self.query
                && names
                    .iter()
                    .any(|name| matches!(name.as_str(), "inputs_for" | "render_print_inputs"))
            || names
                .iter()
                .any(|name| matches!(name.as_str(), "discover_indexed" | "discover_workspace"))
        {
            self.deny(names.join("::"));
        }
        if names
            .windows(2)
            .any(|pair| pair[0] == "io" && matches!(pair[1].as_str(), "Read" | "AsyncReadExt"))
            || !self.adapter && names.iter().any(|name| name == "Disk")
        {
            self.deny(names.join("::"));
        }
        syn::visit::visit_path(self, path);
    }
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if matches!(
            call.method.to_string().as_str(),
            "is_file"
                | "is_dir"
                | "exists"
                | "try_exists"
                | "canonicalize"
                | "metadata"
                | "symlink_metadata"
                | "read_dir"
                | "read_to_string"
                | "read_to_end"
        ) {
            // Adapter calls go through Source/Context; outside it, even Path probes are raw I/O.
            if !self.adapter
                && !matches!(&*call.receiver, syn::Expr::MethodCall(receiver) if receiver.method == "reader")
            {
                self.deny(call.method.to_string());
            }
        }
        syn::visit::visit_expr_method_call(self, call);
    }
    fn visit_expr_unsafe(&mut self, _expression: &'ast syn::ExprUnsafe) {
        self.deny("unsafe analysis");
    }
    fn visit_expr_struct(&mut self, expression: &'ast syn::ExprStruct) {
        if expression
            .path
            .segments
            .last()
            .is_some_and(|s| s.ident == "WalkInput")
        {
            self.deny("constructed WalkInput outside adapter");
        }
        syn::visit::visit_expr_struct(self, expression);
    }
    fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
        if rename.ident == "WalkInput" {
            self.deny("aliased WalkInput constructor");
        }
        syn::visit::visit_use_rename(self, rename);
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if mac.path.segments.last().is_some_and(|segment| {
            matches!(segment.ident.to_string().as_str(), "env" | "option_env")
        }) {
            self.deny("ambient compile-time environment");
        }
        if !matches!(
            mac.path.get_ident().map(ToString::to_string).as_deref(),
            Some("include_str" | "include_bytes" | "file" | "concat")
        ) {
            self.tokens(mac.tokens.clone());
        }
        syn::visit::visit_macro(self, mac);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation(text: &str) -> Option<String> {
        let mut audit = Audit {
            adapter: false,
            query: false,
            frontend: false,
            operation: None,
        };
        audit.visit_file(&syn::parse_file(text).unwrap());
        audit.operation
    }

    #[test]
    fn raw_io_audit_covers_aliases_macros_probes_and_swallowed_reads() {
        for raw in [
            "fn run() { let _ = std::fs::read_to_string(\"secret\").ok(); }",
            "use std::{fs as disk}; fn run() { let _ = disk::read(\"secret\"); }",
            "use std::fs::*; fn run() {}",
            "fn run() { if path.exists() {} }",
            "fn run() { let _ = path.is_file(); }",
            "use walkdir::WalkDir; fn run() {}",
            "macro_rules! raw { () => { std::fs::read(\"secret\") } }",
            "use std::process::Command as C; fn run() {}",
            "use std::io::Read; fn run() {}",
            "fn run() { WalkInput::default(); }",
            "fn run() { WalkInput::parse_files(\"\"); }",
            "use super::WalkInput as Input; fn run() { Input::default(); }",
            "fn run() { let input: WalkInput = std::default::Default::default(); }",
            "fn run() { inputs::inputs_for(name, root); }",
            "fn run() { render_print_inputs(names); }",
            "fn run() { loom_driver::spec::package::discover_workspace(root); }",
            "fn run() { loom_driver::spec::package::discover_indexed(root); }",
            "fn run() { let ambient = std::option_env!(\"HOST_SETTING\"); }",
        ] {
            assert!(operation(raw).is_some(), "{raw}");
        }
        for pure in [
            "fn run(input: &WalkInput) { input.reader().read(path); }",
            "fn run() { let fixture = \"std::fs::read(\\\"x\\\")\"; }",
            "#[cfg(test)] mod tests { use std::fs; fn fixture() {} }",
        ] {
            assert!(operation(pure).is_none(), "{pure}");
        }
    }

    #[test]
    fn disk_exception_does_not_extend_to_tracked_adapter_or_callback_macro() {
        for raw in [
            "impl Source for Tracked<'_, '_> { fn read() { std::fs::read(\"x\"); } }",
            "macro_rules! registration { () => { fn checked() { std::fs::read(\"x\"); } } }",
        ] {
            let mut audit = Audit {
                adapter: true,
                query: false,
                frontend: false,
                operation: None,
            };
            audit.visit_file(&syn::parse_file(raw).unwrap());
            assert!(audit.operation.is_some());
        }
    }
}
