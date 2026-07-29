/*
 * QuickJS atom definitions
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! The predefined atom vocabulary and ordering from `QuickJS`.
//!
//! Ordinals are one-based to preserve the native `QuickJS` atom numbering. Array
//! indices are zero-based and are provided separately to make the distinction
//! explicit at call sites.

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum PredefinedAtomKind {
    String,
    Private,
    Symbol,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PredefinedAtomSpec {
    pub(crate) text: &'static str,
    pub(crate) kind: PredefinedAtomKind,
}

macro_rules! define_predefined_atoms {
    ($($ordinal:literal => $variant:ident, $kind:ident, $text:literal;)+) => {
        /// A `QuickJS` predefined atom.
        ///
        /// The discriminant is the atom's one-based `QuickJS` ordinal.
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(u16)]
        pub enum PredefinedAtom {
            $($variant = $ordinal,)+
        }

        impl PredefinedAtom {
            /// Number of predefined atoms in this `QuickJS` version.
            pub const COUNT: usize = 242;

            /// Every predefined atom in exact ordinal order.
            pub const ALL: [Self; Self::COUNT] = [$(Self::$variant,)+];

            /// Returns the one-based `QuickJS` ordinal.
            #[must_use]
            pub const fn ordinal(self) -> u16 {
                self as u16
            }

            /// Returns the zero-based index into the predefined atom table.
            #[must_use]
            #[allow(
                clippy::cast_lossless,
                reason = "`From<u16> for usize` is not const-stable"
            )]
            pub const fn index(self) -> usize {
                self.ordinal() as usize - 1
            }

            /// Returns the predefined atom at a zero-based table index.
            #[must_use]
            pub const fn from_index(index: usize) -> Option<Self> {
                if index < Self::COUNT {
                    Some(Self::ALL[index])
                } else {
                    None
                }
            }

            /// Returns the predefined atom with a one-based `QuickJS` ordinal.
            #[must_use]
            #[allow(
                clippy::cast_lossless,
                reason = "`From<u16> for usize` is not const-stable"
            )]
            pub const fn from_ordinal(ordinal: u16) -> Option<Self> {
                if ordinal == 0 {
                    None
                } else {
                    Self::from_index(ordinal as usize - 1)
                }
            }

            /// Returns the exact text used to initialize this atom.
            #[must_use]
            pub const fn text(self) -> &'static str {
                self.spec().text
            }

            #[must_use]
            pub(crate) const fn spec(self) -> &'static PredefinedAtomSpec {
                &PREDEFINED_ATOM_SPECS[self.index()]
            }
        }

        pub(crate) const PREDEFINED_ATOM_SPECS: [PredefinedAtomSpec; PredefinedAtom::COUNT] = [
            $(PredefinedAtomSpec {
                text: $text,
                kind: PredefinedAtomKind::$kind,
            },)+
        ];
    };
}

define_predefined_atoms! {
    1 => Null, String, "null";
    2 => False, String, "false";
    3 => True, String, "true";
    4 => If, String, "if";
    5 => Else, String, "else";
    6 => Return, String, "return";
    7 => Var, String, "var";
    8 => This, String, "this";
    9 => Delete, String, "delete";
    10 => Void, String, "void";
    11 => Typeof, String, "typeof";
    12 => New, String, "new";
    13 => In, String, "in";
    14 => Instanceof, String, "instanceof";
    15 => Do, String, "do";
    16 => While, String, "while";
    17 => For, String, "for";
    18 => Break, String, "break";
    19 => Continue, String, "continue";
    20 => Switch, String, "switch";
    21 => Case, String, "case";
    22 => Default, String, "default";
    23 => Throw, String, "throw";
    24 => Try, String, "try";
    25 => Catch, String, "catch";
    26 => Finally, String, "finally";
    27 => FunctionKeyword, String, "function";
    28 => Debugger, String, "debugger";
    29 => With, String, "with";
    30 => Class, String, "class";
    31 => Const, String, "const";
    32 => Enum, String, "enum";
    33 => Export, String, "export";
    34 => Extends, String, "extends";
    35 => Import, String, "import";
    36 => Super, String, "super";
    37 => Implements, String, "implements";
    38 => Interface, String, "interface";
    39 => Let, String, "let";
    40 => Package, String, "package";
    41 => Private, String, "private";
    42 => Protected, String, "protected";
    43 => Public, String, "public";
    44 => Static, String, "static";
    45 => Yield, String, "yield";
    46 => Await, String, "await";
    47 => EmptyString, String, "";
    48 => Keys, String, "keys";
    49 => Size, String, "size";
    50 => Length, String, "length";
    51 => FileName, String, "fileName";
    52 => LineNumber, String, "lineNumber";
    53 => ColumnNumber, String, "columnNumber";
    54 => Message, String, "message";
    55 => Cause, String, "cause";
    56 => Errors, String, "errors";
    57 => Stack, String, "stack";
    58 => Name, String, "name";
    59 => ToString, String, "toString";
    60 => ToLocaleString, String, "toLocaleString";
    61 => ValueOf, String, "valueOf";
    62 => Eval, String, "eval";
    63 => Prototype, String, "prototype";
    64 => Constructor, String, "constructor";
    65 => Configurable, String, "configurable";
    66 => Writable, String, "writable";
    67 => Enumerable, String, "enumerable";
    68 => Value, String, "value";
    69 => Get, String, "get";
    70 => SetProperty, String, "set";
    71 => Of, String, "of";
    72 => Proto, String, "__proto__";
    73 => Undefined, String, "undefined";
    74 => NumberType, String, "number";
    75 => BooleanType, String, "boolean";
    76 => StringType, String, "string";
    77 => ObjectType, String, "object";
    78 => SymbolType, String, "symbol";
    79 => Integer, String, "integer";
    80 => Unknown, String, "unknown";
    81 => ArgumentsIdentifier, String, "arguments";
    82 => Callee, String, "callee";
    83 => Caller, String, "caller";
    84 => EvalMarker, String, "<eval>";
    85 => ReturnMarker, String, "<ret>";
    86 => VarMarker, String, "<var>";
    87 => ArgVarMarker, String, "<arg_var>";
    88 => WithMarker, String, "<with>";
    89 => LastIndex, String, "lastIndex";
    90 => Target, String, "target";
    91 => Index, String, "index";
    92 => Input, String, "input";
    93 => DefineProperties, String, "defineProperties";
    94 => Apply, String, "apply";
    95 => Join, String, "join";
    96 => Concat, String, "concat";
    97 => Split, String, "split";
    98 => Construct, String, "construct";
    99 => GetPrototypeOf, String, "getPrototypeOf";
    100 => SetPrototypeOf, String, "setPrototypeOf";
    101 => IsExtensible, String, "isExtensible";
    102 => PreventExtensions, String, "preventExtensions";
    103 => Has, String, "has";
    104 => DeleteProperty, String, "deleteProperty";
    105 => DefineProperty, String, "defineProperty";
    106 => GetOwnPropertyDescriptor, String, "getOwnPropertyDescriptor";
    107 => OwnKeys, String, "ownKeys";
    108 => Add, String, "add";
    109 => Done, String, "done";
    110 => Next, String, "next";
    111 => Values, String, "values";
    112 => Source, String, "source";
    113 => Flags, String, "flags";
    114 => Global, String, "global";
    115 => Unicode, String, "unicode";
    116 => Raw, String, "raw";
    117 => RawJson, String, "rawJSON";
    118 => NewTarget, String, "new.target";
    119 => ThisActiveFunc, String, "this.active_func";
    120 => HomeObject, String, "<home_object>";
    121 => ComputedField, String, "<computed_field>";
    122 => StaticComputedField, String, "<static_computed_field>";
    123 => ClassFieldsInit, String, "<class_fields_init>";
    124 => Brand, String, "<brand>";
    125 => HashConstructor, String, "#constructor";
    126 => As, String, "as";
    127 => From, String, "from";
    128 => Meta, String, "meta";
    129 => DefaultExport, String, "*default*";
    130 => Star, String, "*";
    131 => Module, String, "Module";
    132 => Then, String, "then";
    133 => Resolve, String, "resolve";
    134 => Reject, String, "reject";
    135 => PromiseIdentifier, String, "promise";
    136 => ProxyIdentifier, String, "proxy";
    137 => Revoke, String, "revoke";
    138 => Async, String, "async";
    139 => Exec, String, "exec";
    140 => Groups, String, "groups";
    141 => Indices, String, "indices";
    142 => Status, String, "status";
    143 => Reason, String, "reason";
    144 => GlobalThis, String, "globalThis";
    145 => BigintType, String, "bigint";
    146 => MinusZero, String, "-0";
    147 => Infinity, String, "Infinity";
    148 => MinusInfinity, String, "-Infinity";
    149 => Nan, String, "NaN";
    150 => HasIndices, String, "hasIndices";
    151 => IgnoreCase, String, "ignoreCase";
    152 => Multiline, String, "multiline";
    153 => DotAll, String, "dotAll";
    154 => Sticky, String, "sticky";
    155 => UnicodeSets, String, "unicodeSets";
    156 => NotEqual, String, "not-equal";
    157 => TimedOut, String, "timed-out";
    158 => Ok, String, "ok";
    159 => ToIsoString, String, "toISOString";
    160 => Alphabet, String, "alphabet";
    161 => LastChunkHandling, String, "lastChunkHandling";
    162 => OmitPadding, String, "omitPadding";
    163 => ToJson, String, "toJSON";
    164 => MaxByteLength, String, "maxByteLength";
    165 => Object, String, "Object";
    166 => Array, String, "Array";
    167 => Error, String, "Error";
    168 => Number, String, "Number";
    169 => String, String, "String";
    170 => Boolean, String, "Boolean";
    171 => Symbol, String, "Symbol";
    172 => Arguments, String, "Arguments";
    173 => Math, String, "Math";
    174 => Json, String, "JSON";
    175 => Date, String, "Date";
    176 => Function, String, "Function";
    177 => GeneratorFunction, String, "GeneratorFunction";
    178 => ForInIterator, String, "ForInIterator";
    179 => RegExp, String, "RegExp";
    180 => ArrayBuffer, String, "ArrayBuffer";
    181 => SharedArrayBuffer, String, "SharedArrayBuffer";
    182 => Uint8ClampedArray, String, "Uint8ClampedArray";
    183 => Int8Array, String, "Int8Array";
    184 => Uint8Array, String, "Uint8Array";
    185 => Int16Array, String, "Int16Array";
    186 => Uint16Array, String, "Uint16Array";
    187 => Int32Array, String, "Int32Array";
    188 => Uint32Array, String, "Uint32Array";
    189 => BigInt64Array, String, "BigInt64Array";
    190 => BigUint64Array, String, "BigUint64Array";
    191 => Float16Array, String, "Float16Array";
    192 => Float32Array, String, "Float32Array";
    193 => Float64Array, String, "Float64Array";
    194 => DataView, String, "DataView";
    195 => BigInt, String, "BigInt";
    196 => WeakRef, String, "WeakRef";
    197 => FinalizationRegistry, String, "FinalizationRegistry";
    198 => Map, String, "Map";
    199 => Set, String, "Set";
    200 => WeakMap, String, "WeakMap";
    201 => WeakSet, String, "WeakSet";
    202 => Iterator, String, "Iterator";
    203 => IteratorHelper, String, "Iterator Helper";
    204 => IteratorConcat, String, "Iterator Concat";
    205 => IteratorWrap, String, "Iterator Wrap";
    206 => MapIterator, String, "Map Iterator";
    207 => SetIterator, String, "Set Iterator";
    208 => ArrayIterator, String, "Array Iterator";
    209 => StringIterator, String, "String Iterator";
    210 => RegExpStringIterator, String, "RegExp String Iterator";
    211 => Generator, String, "Generator";
    212 => Proxy, String, "Proxy";
    213 => Promise, String, "Promise";
    214 => PromiseResolveFunction, String, "PromiseResolveFunction";
    215 => PromiseRejectFunction, String, "PromiseRejectFunction";
    216 => AsyncFunction, String, "AsyncFunction";
    217 => AsyncFunctionResolve, String, "AsyncFunctionResolve";
    218 => AsyncFunctionReject, String, "AsyncFunctionReject";
    219 => AsyncGeneratorFunction, String, "AsyncGeneratorFunction";
    220 => AsyncGenerator, String, "AsyncGenerator";
    221 => EvalError, String, "EvalError";
    222 => RangeError, String, "RangeError";
    223 => ReferenceError, String, "ReferenceError";
    224 => SyntaxError, String, "SyntaxError";
    225 => TypeError, String, "TypeError";
    226 => UriError, String, "URIError";
    227 => InternalError, String, "InternalError";
    228 => AggregateError, String, "AggregateError";
    229 => PrivateBrand, Private, "<brand>";
    230 => SymbolToPrimitive, Symbol, "Symbol.toPrimitive";
    231 => SymbolIterator, Symbol, "Symbol.iterator";
    232 => SymbolMatch, Symbol, "Symbol.match";
    233 => SymbolMatchAll, Symbol, "Symbol.matchAll";
    234 => SymbolReplace, Symbol, "Symbol.replace";
    235 => SymbolSearch, Symbol, "Symbol.search";
    236 => SymbolSplit, Symbol, "Symbol.split";
    237 => SymbolToStringTag, Symbol, "Symbol.toStringTag";
    238 => SymbolIsConcatSpreadable, Symbol, "Symbol.isConcatSpreadable";
    239 => SymbolHasInstance, Symbol, "Symbol.hasInstance";
    240 => SymbolSpecies, Symbol, "Symbol.species";
    241 => SymbolUnscopables, Symbol, "Symbol.unscopables";
    242 => SymbolAsyncIterator, Symbol, "Symbol.asyncIterator";
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{PREDEFINED_ATOM_SPECS, PredefinedAtom, PredefinedAtomKind, PredefinedAtomSpec};

    #[test]
    fn count_boundaries_and_conversions_are_exact() {
        assert_eq!(PredefinedAtom::COUNT, 242);
        assert_eq!(PredefinedAtom::ALL.len(), PredefinedAtom::COUNT);
        assert_eq!(PredefinedAtom::ALL[0], PredefinedAtom::Null);
        assert_eq!(PredefinedAtom::ALL[228], PredefinedAtom::PrivateBrand);
        assert_eq!(PredefinedAtom::ALL[229], PredefinedAtom::SymbolToPrimitive);
        assert_eq!(
            PredefinedAtom::ALL[PredefinedAtom::COUNT - 1],
            PredefinedAtom::SymbolAsyncIterator
        );

        for (index, atom) in PredefinedAtom::ALL.into_iter().enumerate() {
            let ordinal = u16::try_from(index + 1).expect("the predefined table fits in u16");
            assert_eq!(atom.index(), index);
            assert_eq!(atom.ordinal(), ordinal);
            assert_eq!(PredefinedAtom::from_index(index), Some(atom));
            assert_eq!(PredefinedAtom::from_ordinal(ordinal), Some(atom));
            assert_eq!(atom.text(), PREDEFINED_ATOM_SPECS[index].text);
        }

        assert_eq!(PredefinedAtom::from_index(PredefinedAtom::COUNT), None);
        assert_eq!(PredefinedAtom::from_ordinal(0), None);
        let past_last_ordinal =
            u16::try_from(PredefinedAtom::COUNT + 1).expect("the predefined table fits in u16");
        assert_eq!(PredefinedAtom::from_ordinal(past_last_ordinal), None);
    }

    #[test]
    fn namespace_boundaries_and_kinds_are_exact() {
        assert!(
            PREDEFINED_ATOM_SPECS[..228]
                .iter()
                .all(|spec| spec.kind == PredefinedAtomKind::String)
        );
        assert_eq!(
            PREDEFINED_ATOM_SPECS[228],
            PredefinedAtomSpec {
                text: "<brand>",
                kind: PredefinedAtomKind::Private,
            }
        );
        assert!(
            PREDEFINED_ATOM_SPECS[229..]
                .iter()
                .all(|spec| spec.kind == PredefinedAtomKind::Symbol)
        );

        assert_eq!(PredefinedAtom::Brand.ordinal(), 124);
        assert_eq!(PredefinedAtom::PrivateBrand.ordinal(), 229);
        assert_eq!(PredefinedAtom::SymbolToPrimitive.ordinal(), 230);
        assert_eq!(PredefinedAtom::SymbolAsyncIterator.ordinal(), 242);
    }

    #[test]
    fn atom_text_is_unique_within_each_namespace() {
        let mut strings = HashSet::new();
        let mut private_names = HashSet::new();
        let mut symbol_names = HashSet::new();
        let mut namespaced = HashSet::new();

        for spec in PREDEFINED_ATOM_SPECS {
            assert!(namespaced.insert((spec.kind, spec.text)));
            let inserted = match spec.kind {
                PredefinedAtomKind::String => strings.insert(spec.text),
                PredefinedAtomKind::Private => private_names.insert(spec.text),
                PredefinedAtomKind::Symbol => symbol_names.insert(spec.text),
            };
            assert!(
                inserted,
                "duplicate {:?} atom text {:?}",
                spec.kind, spec.text
            );
        }

        assert_eq!(strings.len(), 228);
        assert_eq!(private_names.len(), 1);
        assert_eq!(symbol_names.len(), 13);
        assert!(strings.contains(PredefinedAtom::PrivateBrand.text()));
        assert_eq!(
            PredefinedAtom::Brand.text(),
            PredefinedAtom::PrivateBrand.text()
        );
    }

    #[test]
    fn source_text_is_ascii_with_exact_size() {
        assert!(
            PREDEFINED_ATOM_SPECS
                .iter()
                .all(|spec| spec.text.is_ascii())
        );

        let utf8_bytes = PREDEFINED_ATOM_SPECS
            .iter()
            .map(|spec| spec.text.len())
            .sum::<usize>();
        let utf16_code_units = PREDEFINED_ATOM_SPECS
            .iter()
            .map(|spec| spec.text.encode_utf16().count())
            .sum::<usize>();

        assert_eq!(utf8_bytes, 2_078);
        assert_eq!(utf16_code_units, 2_078);
    }

    #[test]
    fn table_fingerprint_is_stable() {
        const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

        fn hash_byte(hash: &mut u64, byte: u8) {
            *hash ^= u64::from(byte);
            *hash = hash.wrapping_mul(FNV_PRIME);
        }

        let mut fingerprint = FNV_OFFSET_BASIS;
        for (index, spec) in PREDEFINED_ATOM_SPECS.iter().enumerate() {
            let ordinal = u16::try_from(index + 1).expect("the predefined table fits in u16");
            for byte in ordinal.to_le_bytes() {
                hash_byte(&mut fingerprint, byte);
            }

            let kind = match spec.kind {
                PredefinedAtomKind::String => 0,
                PredefinedAtomKind::Private => 1,
                PredefinedAtomKind::Symbol => 2,
            };
            hash_byte(&mut fingerprint, kind);

            let text_len = u16::try_from(spec.text.len()).expect("atom text length fits in u16");
            for byte in text_len.to_le_bytes() {
                hash_byte(&mut fingerprint, byte);
            }
            for byte in spec.text.bytes() {
                hash_byte(&mut fingerprint, byte);
            }
        }

        assert_eq!(fingerprint, 0x5854_a56e_5fa0_02b5);
    }
}
