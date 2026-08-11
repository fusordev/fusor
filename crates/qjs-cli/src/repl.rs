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

use std::io::{BufRead, Write};

use quickjs::{ScriptLimits, evaluate_module, evaluate_script};
use quickjs_runtime::{Runtime, RuntimeLimits};

use crate::{format::format_value, report_error, resolver::NodeLikeResolver};

/// Runs the REPL on stdin/stdout. Returns the process exit code.
pub(crate) fn run() -> u8 {
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

    eprintln!("qjs REPL (host sugar, not a spec module record). .exit or Ctrl-D to quit.");

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut imports: Vec<String> = Vec::new();
    let mut entry_index = 0_u64;
    let mut pending = String::new();
    loop {
        let prompt = if pending.is_empty() { "qjs> " } else { "...> " };
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let line = match lines.next() {
            Some(Ok(line)) => line,
            Some(Err(error)) => {
                eprintln!("qjs: cannot read stdin: {error}");
                return 1;
            }
            None => {
                println!();
                return 0;
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
            match evaluate_module(&mut context, &source, &name, &mut resolver, ScriptLimits::default()) {
                Ok(value) => {
                    println!("{}", format_value(&value));
                    imports.extend(extract_imports(&entry));
                }
                Err(error) => report_error("module entry", &error),
            }
        } else {
            let name = format!("<repl>:{entry_index}");
            match evaluate_script(&mut context, &entry, &name, ScriptLimits::default()) {
                Ok(value) => println!("{}", format_value(&value)),
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
