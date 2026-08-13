//! ESM REPL.
//!
//! A session owns one realm. Each entry is evaluated in one of two ways:
//!
//! - Entries without top-level `import`/`export` syntax are evaluated as
//!   classic scripts; their completion value is printed and successfully
//!   instantiated global bindings persist in the realm for later entries.
//! - Entries with module syntax are evaluated as modules named
//!   `file://<cwd>/__repl_entry_<n>.mjs`, so relative imports resolve against
//!   the current working directory through the [`NodeLikeResolver`]. Every
//!   single-line `import` statement from a successful module entry is
//!   accumulated into a session prefix that is prepended to later module
//!   entries, approximating the incrementally extended module environment of
//!   MODULES.md's "ESM REPL".
//!
//! This is documented host sugar, not a spec module record: each module entry
//! gathers a fresh module graph (the facade is single-shot per call), so
//! imported modules are re-registered and re-evaluated on every module entry,
//! script entries cannot see module-scoped bindings, and only single-line
//! imports are accumulated.

use quickjs::{ScriptLimits, evaluate_preloaded_module_graph, evaluate_script};
use quickjs_runtime::{Runtime, RuntimeLimits};

use crate::{format::format_value, report_error, resolver::NodeLikeResolver};

/// Runs the REPL on stdin/stdout. Returns the process exit code.
///
/// Module entries load their static graph asynchronously on the caller's
/// Tokio runtime; script entries stay fully synchronous.
pub(crate) async fn run() -> u8 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => crate::resolver::normalize_path(&cwd),
        Err(error) => {
            eprintln!("qjs: cannot determine the current directory: {error}");
            return 2;
        }
    };
    let mut runtime = match Runtime::try_new(RuntimeLimits::default()) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("qjs: cannot create the runtime: {error}");
            return 2;
        }
    };
    let realm = match runtime.create_realm() {
        Ok(realm) => realm,
        Err(error) => {
            eprintln!("qjs: cannot create a realm: {error}");
            return 2;
        }
    };
    let mut context = match runtime.context(&realm) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("qjs: cannot create a context: {error}");
            return 2;
        }
    };
    let mut resolver = NodeLikeResolver::new(cwd.clone(), vec!["qjs".to_owned()]);
    let entry_prefix = format!("file://{}", cwd.display());

    // Host `print`: renders each argument like the REPL's completion printer
    // and writes it to stdout. Installed as a global so both script and module
    // entries can reach it.
    let print = match context.create_host_function("print", |ctx, call| {
        let rendered: Vec<String> = call
            .arguments()
            .iter()
            .map(crate::format::format_argument)
            .collect();
        println!("{}", rendered.join(" "));
        Ok(ctx.undefined_value())
    }) {
        Ok(function) => function,
        Err(error) => {
            eprintln!("qjs: cannot install print: {error}");
            return 1;
        }
    };
    if let Err(error) = context.set_global("print", print.as_value()) {
        eprintln!("qjs: cannot install the print global: {error}");
        return 1;
    }

    eprintln!("qjs REPL (host sugar, not a spec module record). .exit or Ctrl-D to quit.");

    let mut editor = match rustyline::DefaultEditor::new() {
        Ok(editor) => editor,
        Err(error) => {
            eprintln!("qjs: cannot initialize the line editor: {error}");
            return 1;
        }
    };
    let mut imports: Vec<String> = Vec::new();
    let mut entry_index = 0_u64;
    let mut pending = String::new();
    loop {
        let prompt = if pending.is_empty() { "qjs> " } else { "...> " };
        let line = match editor.readline(prompt) {
            Ok(line) => {
                let _ = editor.add_history_entry(line.as_str());
                line
            }
            Err(rustyline::error::ReadlineError::Interrupted) => continue,
            Err(rustyline::error::ReadlineError::Eof) => {
                println!();
                return 0;
            }
            Err(error) => {
                eprintln!("qjs: cannot read input: {error}");
                return 1;
            }
        };
        if pending.is_empty() && line.trim() == ".exit" {
            return 0;
        }
        pending.push_str(&line);
        pending.push('\n');
        if !braces_balanced(&pending) {
            continue;
        }
        let entry = std::mem::take(&mut pending);
        if entry.trim().is_empty() {
            continue;
        }
        entry_index += 1;
        if has_module_syntax(&entry) {
            let mut source = imports.join("\n");
            if !source.is_empty() {
                source.push('\n');
            }
            source.push_str(&entry);
            let name = format!("{entry_prefix}/__repl_entry_{entry_index}.mjs");
            let limits = ScriptLimits::default();
            let result = match crate::loader::gather_static_graph(&resolver, &source, &name, limits).await {
                Ok(edges) => evaluate_preloaded_module_graph(&mut context, &source, &name, edges, limits),
                Err(error) => Err(error),
            };
            match result {
                Ok(value) => {
                    if let Err(error) =
                        crate::imports::drain_pending_imports(&mut context, &mut resolver, limits).await
                    {
                        report_error("module entry", &error);
                    }
                    // A top-level-await entry settles asynchronously while the
                    // drain runs its continuations; a rejection recorded on
                    // the entry's module record is its evaluation failure.
                    if let Some(error) = quickjs::module_evaluation_error(&context, &name) {
                        report_error("module entry", &error);
                    }
                    println!("{}", format_value(&value));
                    imports.extend(extract_imports(&entry));
                }
                Err(error) => report_error("module entry", &error),
            }
        } else {
            let name = format!("<repl>:{entry_index}");
            let limits = ScriptLimits::default();
            match evaluate_script(&mut context, &entry, &name, limits) {
                Ok(value) => {
                    // Script entries may park dynamic `import()`/`import.defer()`
                    // loads; settle them so their reactions run before the next
                    // entry.
                    if let Err(error) =
                        crate::imports::drain_pending_imports(&mut context, &mut resolver, limits).await
                    {
                        report_error("script entry", &error);
                    }
                    println!("{}", format_value(&value));
                }
                Err(error) => report_error("script entry", &error),
            }
        }
    }
}

/// Returns whether an entry looks like it uses module syntax. Heuristic: any
/// line starting with a static `import`/`export` statement. Dynamic
/// `import(` is an expression and stays on the script path.
fn has_module_syntax(source: &str) -> bool {
    source.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("import ")
            || trimmed.starts_with("import{")
            || trimmed.starts_with("export ")
            || trimmed.starts_with("export{")
    })
}

/// Collects complete single-line `import` statements for the session prefix.
fn extract_imports(source: &str) -> Vec<String> {
    source
        .lines()
        .map(str::trim)
        .filter(|line| {
            (line.starts_with("import ") || line.starts_with("import{")) && line.ends_with(';')
        })
        .map(ToOwned::to_owned)
        .collect()
}

/// Basic multiline heuristic: brackets are balanced once every `(`/`[`/`{`
/// has a match, ignoring brackets inside strings, template literals, and
/// comments. Extra closing brackets count as balanced so the resulting parse
/// error surfaces from the engine instead of hanging the prompt.
fn braces_balanced(source: &str) -> bool {
    let mut depth = 0_i32;
    let mut chars = source.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '"' | '\'' | '`' => {
                let quote = character;
                let mut escaped = false;
                for inner in chars.by_ref() {
                    if escaped {
                        escaped = false;
                    } else if inner == '\\' {
                        escaped = true;
                    } else if inner == quote {
                        break;
                    }
                }
            }
            '/' => match chars.peek() {
                Some('/') => {
                    for inner in chars.by_ref() {
                        if inner == '\n' {
                            break;
                        }
                    }
                }
                Some('*') => {
                    let mut previous = '\0';
                    for inner in chars.by_ref() {
                        if previous == '*' && inner == '/' {
                            break;
                        }
                        previous = inner;
                    }
                }
                _ => {}
            },
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_heuristic_ignores_strings_and_comments() {
        assert!(!braces_balanced("function f() {"));
        assert!(braces_balanced("function f() {\n}"));
        assert!(braces_balanced("const s = \"} {\";"));
        assert!(braces_balanced("const t = `} ${ 1 }`; // {"));
        assert!(braces_balanced("/* { */ (1)"));
        assert!(braces_balanced("}"));
    }

    #[test]
    fn module_syntax_detection_covers_imports_and_exports() {
        assert!(has_module_syntax("import { a } from './a.mjs';"));
        assert!(has_module_syntax("import './side.mjs';"));
        assert!(has_module_syntax("export const x = 1;"));
        assert!(!has_module_syntax("const x = 1;"));
        assert!(!has_module_syntax("import('./dynamic.mjs');"));
    }

    #[test]
    fn import_extraction_keeps_complete_single_line_imports() {
        let imports = extract_imports(
            "import { a } from './a.mjs';\nimport './b.mjs'\nconst x = 1;\nimport {\n",
        );
        assert_eq!(imports, vec!["import { a } from './a.mjs';".to_owned()]);
    }
}
