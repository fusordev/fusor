# Upstream provenance

| Field | Value |
| --- | --- |
| Project | QuickJS |
| Authors | Fabrice Bellard and Charlie Gordon |
| Release | `2026-06-04` |
| Release page | <https://bellard.org/quickjs/> |
| Source archive | <https://bellard.org/quickjs/quickjs-2026-06-04.tar.xz> |
| SHA-256 | `b376e839b322978313d929fd20663b11ba58b75df5a46c126dd19ea2fa70ad2a` |
| Extras archive | <https://bellard.org/quickjs/quickjs-extras-2026-06-04.tar.xz> |
| Extras SHA-256 | `11549a45b25b055946eeac2a0064399297dcf80062c6c07b644e0bc5eb329817` |
| Official Git | <https://github.com/bellard/quickjs> |
| Git `master` observed | `04be246001599f5995fa2f2d8c91a0f198d3f34c` |
| Test262 revision | `5c8206929d81b2d3d727ca6aac56c18358c8d790` |
| License | MIT |

The release archive is the normative source target. The moving Git branch is
recorded only for traceability and must not silently change compatibility.

The Test262 revision comes from the release `Makefile`. Conformance runs must
also apply the release's `tests/test262.patch`, use `test262.conf`, and compare
against `test262_errors.txt`; the revision alone is not the upstream baseline.

With the release's `SHORT_OPCODES=1` configuration, `quickjs-opcode.h` defines
244 final table entries (including byte zero's reserved `invalid` sentinel) and
19 compiler-temporary entries. The translated metadata's canonical FNV-1a
fingerprint is `37d5ab885a37011f`, computed over ordered
`domain|mnemonic|size|pops|pushes|operand-format` rows.

The release's `quickjs-atom.h` defines 242 predefined atoms in order: 228
strings, one private brand, and 13 well-known symbols. Their texts contain
2,078 ASCII bytes (and therefore 2,078 UTF-16 code units). The translated
table's canonical FNV-1a fingerprint is `5854a56e5fa002b5`, computed over each
one-based ordinal, namespace tag, text length, and text bytes.

Upstream source and binaries may be used as development oracles. They are not
vendored, linked, or required to build, test, or use the Rust implementation.
