# QuickJS local patch

This directory is the published `sdd` 4.8.8 crate, locally patched for the
pure-Rust QuickJS port.

- Published crate SHA-256:
  `1836bad8bdc9c6d665b63202da3d9c6d60ed1e597cae63620e21ebf89a3595a9`
- Published VCS revision:
  `abfa4308c24062fa91a571658a35a6c69cc8cf7b`
- License: Apache-2.0

The local delta replaces five raw allocation sites that could write through a
null pointer with RAII `Box<MaybeUninit<_>>` allocation, retains allocation
ownership if value construction unwinds, and makes the collector arena return
`NonNull<Collector>`. The private collector's identifier allocation remains a
checked raw allocation because its surrounding unwind boundary intentionally
falls back to a backup garbage bag.

No API or intended reclamation algorithm is changed.
