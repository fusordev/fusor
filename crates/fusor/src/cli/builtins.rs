//! Minimal `node:` builtin modules, generated as JavaScript source strings.
//!
//! These are non-normative host sugar: a small, clearly documented subset of
//! the Node builtin surface, implemented in plain JavaScript so the runtime
//! needs no host-function support. Paths are POSIX-style (`/` separators);
//! `node:path` and `node:process` observe the host current working directory
//! captured when the resolver was constructed.
//!
//! Note on structure: the current runtime only installs module-level function
//! templates whose closures capture module-scope *data* bindings — any global
//! reference (`Error`, `Object`, `JSON`, ...) or sibling-function capture in a
//! module-level function fails installation. Each builtin therefore builds its
//! API inside an IIFE evaluated in the module body (runtime-created closures,
//! which may reference globals freely) and exports the resulting bindings.

use std::path::Path;

/// Bare specifiers (in addition to their `node:`-prefixed form) that resolve
/// against the builtin table.
pub(crate) const NAMES: [&str; 3] = ["assert", "path", "process"];

/// Returns whether `name` is a known builtin (without the `node:` prefix).
pub(crate) fn is_builtin(name: &str) -> bool {
    NAMES.contains(&name)
}

/// Returns the generated module source for builtin `name` (no `node:` prefix).
pub(crate) fn source(name: &str, cwd: &Path, argv: &[String]) -> Option<String> {
    let cwd = js_string(&cwd.to_string_lossy());
    match name {
        "assert" => Some(ASSERT_SOURCE.to_owned()),
        "path" => Some(PATH_SOURCE.replace("__QJS_CWD__", &cwd)),
        "process" => Some(process_source(&cwd, argv)),
        _ => None,
    }
}

/// Quotes a Rust string as a double-quoted JavaScript string literal.
fn js_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(quoted, "\\u{:04x}", c as u32);
            }
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

fn process_source(cwd: &str, argv: &[String]) -> String {
    let argv = argv
        .iter()
        .map(|argument| js_string(argument))
        .collect::<Vec<_>>()
        .join(", ");
    PROCESS_SOURCE
        .replace("__QJS_ARGV__", &argv)
        .replace("__QJS_CWD__", cwd)
        .replace("__QJS_PLATFORM__", std::env::consts::OS)
}

const ASSERT_SOURCE: &str = r"
const api = (function () {
  function describe(value) {
    if (typeof value === 'string') return JSON.stringify(value);
    try {
      const json = JSON.stringify(value);
      return json === undefined ? String(value) : json;
    } catch (error) {
      return String(value);
    }
  }
  function fail(text) {
    throw new Error(text === undefined ? 'assertion failed' : text);
  }
  function ok(value, text) {
    if (!value) {
      throw new Error(text === undefined ? 'expected a truthy value, got ' + describe(value) : text);
    }
  }
  function strictEqual(actual, expected, text) {
    if (!Object.is(actual, expected)) {
      throw new Error(text === undefined ? describe(actual) + ' !== ' + describe(expected) : text);
    }
  }
  function notStrictEqual(actual, expected, text) {
    if (Object.is(actual, expected)) {
      throw new Error(text === undefined ? 'values are strictly equal: ' + describe(actual) : text);
    }
  }
  function equal(actual, expected, text) {
    if (actual != expected) {
      throw new Error(text === undefined ? describe(actual) + ' != ' + describe(expected) : text);
    }
  }
  function deepStrictEqual(actual, expected, text) {
    if (!deepEqual(actual, expected)) {
      throw new Error(
        text === undefined ? describe(actual) + ' not deep-equal to ' + describe(expected) : text,
      );
    }
  }
  function deepEqual(left, right) {
    if (Object.is(left, right)) return true;
    if (typeof left !== 'object' || typeof right !== 'object' || left === null || right === null) {
      return false;
    }
    const leftKeys = Object.keys(left);
    const rightKeys = Object.keys(right);
    if (leftKeys.length !== rightKeys.length) return false;
    for (let i = 0; i < leftKeys.length; i++) {
      const key = leftKeys[i];
      if (!Object.prototype.hasOwnProperty.call(right, key)) return false;
      if (!deepEqual(left[key], right[key])) return false;
    }
    return true;
  }
  function throws(fn, text) {
    let threw = false;
    try { fn(); } catch (error) { threw = true; }
    if (!threw) throw new Error(text === undefined ? 'expected the function to throw' : text);
  }
  return { fail, ok, strictEqual, notStrictEqual, equal, deepStrictEqual, throws };
})();
export default api;
export const fail = api.fail;
export const ok = api.ok;
export const strictEqual = api.strictEqual;
export const notStrictEqual = api.notStrictEqual;
export const equal = api.equal;
export const deepStrictEqual = api.deepStrictEqual;
export const throws = api.throws;
";

const PATH_SOURCE: &str = r"
const api = (function () {
  const CWD = __QJS_CWD__;
  function isAbsolute(p) { return p.length > 0 && p.charAt(0) === '/'; }
  function normalize(p) {
    const absolute = isAbsolute(p);
    const parts = p.split('/');
    const out = [];
    for (let i = 0; i < parts.length; i++) {
      const part = parts[i];
      if (part === '' || part === '.') continue;
      if (part === '..') {
        if (out.length > 0 && out[out.length - 1] !== '..') out.pop();
        else if (!absolute) out.push('..');
        continue;
      }
      out.push(part);
    }
    let result = (absolute ? '/' : '') + out.join('/');
    if (result === '') result = absolute ? '/' : '.';
    return result;
  }
  function join() {
    const parts = [];
    for (let i = 0; i < arguments.length; i++) {
      const part = arguments[i];
      if (typeof part !== 'string') throw new TypeError('path.join segments must be strings');
      if (part.length > 0) parts.push(part);
    }
    if (parts.length === 0) return '.';
    return normalize(parts.join('/'));
  }
  function dirname(p) {
    if (typeof p !== 'string') throw new TypeError('path.dirname argument must be a string');
    let end = p.length - 1;
    while (end > 0 && p.charAt(end) === '/') end--;
    const slash = p.lastIndexOf('/', end);
    if (slash < 0) return '.';
    if (slash === 0) return '/';
    return p.slice(0, slash);
  }
  function basename(p, ext) {
    if (typeof p !== 'string') throw new TypeError('path.basename argument must be a string');
    let end = p.length;
    while (end > 0 && p.charAt(end - 1) === '/') end--;
    const start = p.lastIndexOf('/', end - 1) + 1;
    let base = p.slice(start, end);
    if (typeof ext === 'string' && ext.length > 0 &&
        base.length > ext.length && base.slice(base.length - ext.length) === ext) {
      base = base.slice(0, base.length - ext.length);
    }
    return base;
  }
  function extname(p) {
    const base = basename(p);
    const dot = base.lastIndexOf('.');
    if (dot <= 0) return '';
    return base.slice(dot);
  }
  function resolve() {
    let resolved = '';
    for (let i = arguments.length - 1; i >= -1; i--) {
      const part = i >= 0 ? arguments[i] : CWD;
      if (typeof part !== 'string') throw new TypeError('path.resolve segments must be strings');
      if (part === '') continue;
      resolved = resolved === '' ? part : part + '/' + resolved;
      if (part.charAt(0) === '/') break;
    }
    return normalize(resolved);
  }
  return {
    isAbsolute, normalize, join, dirname, basename, extname, resolve,
    sep: '/', delimiter: ':',
  };
})();
export default api;
export const isAbsolute = api.isAbsolute;
export const normalize = api.normalize;
export const join = api.join;
export const dirname = api.dirname;
export const basename = api.basename;
export const extname = api.extname;
export const resolve = api.resolve;
export const sep = api.sep;
export const delimiter = api.delimiter;
";

const PROCESS_SOURCE: &str = r"
const api = (function () {
  const argv = [__QJS_ARGV__];
  const env = {};
  const platform = '__QJS_PLATFORM__';
  const cwdValue = __QJS_CWD__;
  function cwd() { return cwdValue; }
  return { argv, env, platform, cwd };
})();
export default api;
export const argv = api.argv;
export const env = api.env;
export const platform = api.platform;
export const cwd = api.cwd;
";
