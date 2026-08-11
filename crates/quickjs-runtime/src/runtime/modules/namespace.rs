//! Module namespace exotic objects.

use super::ModuleRecordId;
use crate::object::OwnProperty;
use crate::runtime::{BindingCell, ObjectId, Runtime, SlotValue, StoredValue};
use crate::{PropertyKey, PropertyLayout};

/// Runtime state for a module namespace exotic object.
#[derive(Debug)]
pub(crate) struct ModuleNamespaceState {
    /// The module whose exports this namespace exposes.
    pub(crate) module: ModuleRecordId,
    /// Sorted export names (UTF-8) → resolved target (module + cell).
    pub(crate) exports: Vec<(Box<[u8]>, NamespaceExport)>,
}

/// A namespace export entry (ECMA-262 ResolvedBinding shape).
#[derive(Clone, Copy, Debug)]
pub(crate) enum NamespaceExport {
    /// A live binding: target module + binding cell.
    Binding {
        module: ModuleRecordId,
        cell: crate::runtime::BindingCellId,
    },
    /// An `export * as name` re-export: the target module, whose namespace
    /// object is realized after this namespace is installed (self-references
    /// and cycles resolve once creation unwinds).
    Namespace {
        module: ModuleRecordId,
    },
}

impl Runtime {
    /// Returns whether `object` is a module namespace exotic object.
    pub(crate) fn is_module_namespace_object(
        &self,
        object: ObjectId,
    ) -> Result<bool, crate::EngineFault> {
        self.objects
            .get(object)
            .map(|object| object.module_namespace_state().is_some())
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })
    }

    /// Implements the export branch of the namespace `[[GetOwnProperty]]`
    /// (ECMA-262 10.4.6.3).
    ///
    /// A string key naming an export resolves to a data descriptor whose value
    /// is the current value of the target binding, keeping exports live.
    /// Symbol keys and unknown strings fall through to the ordinary record
    /// (which carries `@@toStringTag` and the key-shape placeholders for
    /// `[[OwnPropertyKeys]]`).
    pub(crate) fn module_namespace_export_property(
        &self,
        object: ObjectId,
        key: &PropertyKey,
    ) -> Result<Option<OwnProperty>, crate::EngineFault> {
        let Some(state) = self
            .objects
            .get(object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })?
            .module_namespace_state()
        else {
            return Ok(None);
        };
        let Some(name) = namespace_key_bytes(key) else {
            return Ok(None);
        };
        let Some((_, export)) = state
            .exports
            .iter()
            .find(|(export_name, _)| export_name.as_ref() == name.as_slice())
        else {
            return Ok(None);
        };
        let value = match export {
            NamespaceExport::Namespace { module } => {
                // Realized when the owning namespace was installed; a missing
                // object means namespace creation never completed.
                let namespace = self
                    .modules
                    .get(*module)
                    .and_then(|record| record.namespace_object)
                    .ok_or(crate::EngineFault::RuntimeInvariant {
                        message: "namespace re-export target was never realized",
                    })?;
                StoredValue::Object(namespace)
            }
            NamespaceExport::Binding { cell, .. } => {
                let cell = BindingCell::resolve_forward(self, *cell)?;
                match &self
                    .cells
                    .get(cell)
                    .ok_or(crate::EngineFault::StaleHeapEdge {
                        edge: "module namespace binding cell",
                        index: cell.index(),
                        generation: cell.generation(),
                    })?
                    .value
                {
                    SlotValue::Value(value) => value.duplicate(),
                    // ECMA-262 throws a ReferenceError here; this read path cannot
                    // raise a JavaScript exception, so a binding still in its temporal
                    // dead zone reads as `undefined`. Cyclic namespace reads during
                    // the dead zone are rejected through the importing binding's own
                    // checked access instead.
                    SlotValue::Uninitialized => StoredValue::Undefined,
                }
            }
        };
        Ok(Some(OwnProperty::Data {
            layout: PropertyLayout::data(true, true, false),
            value,
        }))
    }
}

/// Converts a property key to the UTF-8 export-name form, or `None` for keys
/// that can never name an export (symbols).
fn namespace_key_bytes(key: &PropertyKey) -> Option<Vec<u8>> {
    if let Some(index) = key.as_index() {
        return Some(index.get().to_string().into_bytes());
    }
    let description = key.as_atom()?.description()?;
    description.to_utf8_lossy().ok().map(String::into_bytes)
}
