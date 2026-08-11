//! Per-module `import.meta` object materialization.
//!
//! ECMA-262 leaves `import.meta` population to the host
//! (`HostGetImportMetaProperties`). The runtime creates one ordinary object
//! (with the realm's `%Object.prototype%`) per module on first access, caches
//! it on the module record so repeated reads are identity-stable, and consults
//! the installed [`ImportMetaHook`] for its observable properties. Without a
//! hook, `url` is the canonical module key and `import.meta.resolve` applies
//! [`super::default_import_meta_resolve`].

use super::ModuleRecordId;
use crate::object::{HeapObject, ObjectRecord};
use crate::runtime::{
    FunctionImplementation, HeapFunction, NativeFunction, NativeFunctionKind, ObjectId, Runtime,
    StoredValue,
};
use crate::string::JsString;
use crate::runtime::HeapReference;
use crate::{ExecutionError, PredefinedAtom, PropertyLayout};

/// Returns the module's lazily materialized `import.meta` object.
pub(crate) fn get_or_create_import_meta(
    runtime: &mut Runtime,
    module: ModuleRecordId,
) -> Result<ObjectId, ExecutionError> {
    if let Some(meta) = runtime
        .modules
        .get(module)
        .and_then(|record| record.meta_object)
    {
        return Ok(meta);
    }
    let (realm, key) = {
        let record = runtime
            .modules
            .get(module)
            .ok_or(crate::EngineFault::RuntimeInvariant {
                message: "import_meta module record is stale",
            })?;
        (record.realm, record.key.clone())
    };
    let url = match runtime.import_meta_hook() {
        Some(hook) => hook.url(&key),
        None => key.as_str().to_owned(),
    };

    let function_prototype = HeapReference::Function(runtime.realm_function_prototype(realm)?);
    let object_prototype = HeapReference::Object(runtime.realm_object_prototype(realm)?);
    let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
    let resolve_key = runtime.predefined_property_key(PredefinedAtom::Resolve);
    let url_key = runtime.property_key_from_string(&JsString::from_utf8("url")?)?;

    // The `resolve` function is published before the meta object that
    // references it; both records are fully built before insertion, so a
    // failure cannot leave a partially initialized property layout behind.
    let mut resolve_record = ObjectRecord::empty(Some(function_prototype));
    resolve_record.try_reserve_data(2).map_err(|_| {
        ExecutionError::AllocationFailed {
            resource: crate::RuntimeResource::ObjectProperties,
            additional: 2,
        }
    })?;
    resolve_record
        .append_data(
            length_key,
            PropertyLayout::data(false, false, true),
            StoredValue::Number(crate::JsNumber::from_i32(1)),
        )
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: crate::RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    resolve_record
        .append_data(
            name_key,
            PropertyLayout::data(false, false, true),
            StoredValue::String(JsString::from_utf8("resolve")?),
        )
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: crate::RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    let resolve = runtime
        .insert_heap_function(HeapFunction {
            implementation: FunctionImplementation::Native(NativeFunction {
                realm,
                kind: NativeFunctionKind::ImportMetaResolve,
            }),
            object: resolve_record,
            public_roots: 0,
        })
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: crate::RuntimeResource::HeapFunctions,
            additional: 1,
        })?;

    // Host-populated properties follow CreateDataProperty semantics
    // (writable, enumerable, configurable), per `HostGetImportMetaProperties`.
    let mut record = ObjectRecord::empty(Some(object_prototype));
    record
        .try_reserve_data(2)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: crate::RuntimeResource::ObjectProperties,
            additional: 2,
        })?;
    record
        .append_data(
            url_key,
            PropertyLayout::data(true, true, true),
            StoredValue::String(JsString::from_utf8(&url)?),
        )
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: crate::RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    record
        .append_data(
            resolve_key,
            PropertyLayout::data(true, true, true),
            StoredValue::Function(resolve),
        )
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: crate::RuntimeResource::ObjectProperties,
            additional: 1,
        })?;

    runtime
        .objects
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: crate::RuntimeResource::HeapObjects,
            additional: 1,
        })?;
    let meta = runtime
        .insert_heap_object(HeapObject::ordinary(record))
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: crate::RuntimeResource::HeapObjects,
            additional: 1,
        })?;
    runtime.object_properties = runtime.object_properties.saturating_add(4);
    runtime
        .modules
        .get_mut(module)
        .expect("module exists")
        .meta_object = Some(meta);
    runtime.collection_pending = true;
    Ok(meta)
}
