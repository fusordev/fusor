# Vendored Oxc provenance

This directory contains the exact Oxc-family package closure selected by the
workspace lockfile. It is a selective source override, not a complete Cargo
registry mirror:

- all 17 packages below resolve through workspace `[patch.crates-io]` entries;
- unrelated Rust dependencies remain registry-backed;
- no `[source.crates-io] replace-with` configuration may point at this
  directory.

The package directories were produced with Cargo's versioned vendor layout
from the original crates.io archives. Each retains `Cargo.toml.orig`,
`.cargo_vcs_info.json`, and `.cargo-checksum.json`. The registry checksums
below were independently checked against both the downloaded `.crate` archive
and the pre-vendoring `Cargo.lock`.

## Sources

- Oxc 0.142.0: <https://github.com/oxc-project/oxc/tree/fc702c1fa9f0412d06ec6908b58cd395b826cf7f>
- oxc-miette 3.0.1: <https://github.com/oxc-project/oxc-miette/tree/40969d769cdd0dcb8223883ab61327a6b2e3e961>
- oxc_index 5.0.0: <https://github.com/oxc-project/oxc-index-vec/tree/8e09fe324eb6df02f56e4eacdfac958930300380>

## Exact packages

| Package | Version | Original registry SHA-256 | License | VCS path |
|---|---:|---|---|---|
| `oxc-miette` | 3.0.1 | `2e0df30faa68797917ca4263e7a2f889ec829e4da2dcb3d6dc752f7a494180f3` | Apache-2.0 | repository root |
| `oxc-miette-derive` | 3.0.1 | `acc072d11d45ebe7801459b4e829184ba0934d68027fdc51d327335b53a95a49` | Apache-2.0 | `miette-derive` |
| `oxc_allocator` | 0.142.0 | `ae912912216c33ad0fb27a544160f3833a5e92a1216d7c4b9aa79a94b39aee98` | MIT | `crates/oxc_allocator` |
| `oxc_ast` | 0.142.0 | `6f0d567d1a93714e4b6b14eabe76c45581a25f418f47c297911a43fe4ee99615` | MIT | `crates/oxc_ast` |
| `oxc_ast_macros` | 0.142.0 | `a3fff81fa6fffffcf13826a6bc352e402a9197ac88ca286e2b09a0c538904708` | MIT | `crates/oxc_ast_macros` |
| `oxc_ast_visit` | 0.142.0 | `d13c8c77043e21d491840a6d52aa00f3ed1432662b096665b6ad702626f3ec5e` | MIT | `crates/oxc_ast_visit` |
| `oxc_data_structures` | 0.142.0 | `2d7793b9a760ff426782915100e1f91b4a653873a5b681576214a6eb495d498a` | MIT | `crates/oxc_data_structures` |
| `oxc_diagnostics` | 0.142.0 | `a401193a5bc7f8acc2665763ea5876cad4a64e4e85c55133376620906e34b9e1` | MIT | `crates/oxc_diagnostics` |
| `oxc_ecmascript` | 0.142.0 | `9f099596aa0f5cb1aac154f7833abc02e0205aac19ce8b2bd21ee1e52973de61` | MIT | `crates/oxc_ecmascript` |
| `oxc_estree` | 0.142.0 | `f2284782be0ac819aa56b815e6448e140e9f4f2c917917ac4ffa8bd7ccc35a3b` | MIT | `crates/oxc_estree` |
| `oxc_index` | 5.0.0 | `191884bee6c3744909a51acc7d78d4ae370d817b25875b10642f632327b6296e` | MIT | repository root |
| `oxc_parser` | 0.142.0 | `ff65aedf1d4364457427d461b827cc43544c5275f2693144a4fdb5fe5c26fbd9` | MIT | `crates/oxc_parser` |
| `oxc_regular_expression` | 0.142.0 | `69bc0cfbcd78e42d9aaa72eccb77e62b4b85b671f5d08ff8430aeea9816a8273` | MIT | `crates/oxc_regular_expression` |
| `oxc_semantic` | 0.142.0 | `0293d641451efb4d29bd17ab44e413f8d5daa10e9dc541c7b30ddcf136fd0a4d` | MIT | `crates/oxc_semantic` |
| `oxc_span` | 0.142.0 | `890589be8c87e7c0f7a1f0331a9b8d362ebcd3e543125815daa942fd550b0121` | MIT | `crates/oxc_span` |
| `oxc_str` | 0.142.0 | `91f2b9a92f74a8231f21fccfb4eef937d266a7323d9597ccdcd74c7ad0f951b1` | MIT | `crates/oxc_str` |
| `oxc_syntax` | 0.142.0 | `e54e5c73453878e194df8d77d5cdc9ef525fec8ad25c547cc87162318eda6adb` | MIT | `crates/oxc_syntax` |

The two Apache-2.0 packages contain their packaged `LICENSE` files. The
crates.io packages for the MIT projects omit their repository-root license, so
the exact pinned files are preserved separately:

- `LICENSES/OXC-MIT`, SHA-256
  `95ced5ecf1133fbf41d409b5555c86c344f83f3b019926057ddbc07cfdcc27b3`;
- `LICENSES/OXC-INDEX-MIT`, SHA-256
  `95ced5ecf1133fbf41d409b5555c86c344f83f3b019926057ddbc07cfdcc27b3`.

## Verification

After any dependency change, first refresh `Cargo.lock` normally, then prove
the locked graph resolves the exact package set to local paths:

```console
CARGO_HOME=/private/tmp/quickjs-cargo-home \
  cargo metadata --locked --offline --format-version 1 \
  > /private/tmp/quickjs-oxc-metadata.json
```

For every package whose name starts with `oxc`, the metadata must satisfy:

1. the exact `name@version` set is the 17 rows above;
2. `source` is `null`;
3. `manifest_path` is
   `<workspace>/vendor/oxc/crates/<name>-<version>/Cargo.toml`.

Then compile and test the selected sources:

```console
CARGO_HOME=/private/tmp/quickjs-cargo-home \
  cargo test --workspace --all-features --locked --offline
```

An empty-Cargo-home air-gapped build is not claimed: unrelated dependencies
are intentionally not vendored.

## Local modifications

The package checksum manifests describe the original published archives and
must not be regenerated to hide local changes. Project-specific Oxc changes
must be reviewable in Git, documented here with their compatibility reason,
and covered by focused QuickJS differential or safety tests.

The following project-owned compatibility extensions are applied after the
published-package baseline:

- `oxc_parser`: `ParseOptions::allow_top_level_await` enables the host's
  QuickJS async-global Script goal without changing the source type to Module.
- `oxc_semantic`: `SemanticBuilder::with_forced_strict` marks the root Script
  scope strict for the corresponding QuickJS evaluation flag without
  rewriting source text.
- `oxc_semantic`: the statistics pre-pass and non-CFG semantic builder traverse
  `BinaryExpression` and `LogicalExpression` chains with explicit task stacks.
  This preserves generated visitor enter/child/leave order, semantic parent
  IDs, reference resolution, and syntax checking while accepting the long
  left-deep expressions supported by QuickJS 2026-06-04. CFG traversal is
  unchanged.
