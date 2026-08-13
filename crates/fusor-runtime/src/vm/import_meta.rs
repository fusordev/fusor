//! `import.meta.resolve` host-resolution boundary.
//!
//! The function is created per module together with the `import.meta` object
//! (see `runtime::modules::get_or_create_import_meta`). It resolves the
//! specifier against the receiver meta object's `url` own property, delegating
//! to the installed [`crate::ImportMetaHook`] or, without one, to
//! [`crate::runtime::default_import_meta_resolve`].

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) fn import_meta_resolve(
    runtime: &mut Runtime,
    realm: RealmId,
    receiver: &StoredValue,
    specifier: StoredValue,
    origin: JsStackFrame,
) -> Result<NativeDispatch, NativeFailure> {
    // ToString(specifier), restricted to primitives: invoking user conversion
    // code would require the resumable primitive-conversion machine, which a
    // one-shot host-resolution call does not admit.
    let specifier = match specifier {
        StoredValue::Object(_) | StoredValue::Function(_) => {
            return import_meta_resolve_type_error(
                realm,
                origin,
                "import.meta.resolve specifier must be a primitive value",
            );
        }
        value => operator_primitive_to_string(value, realm, &origin)?,
    };
    let specifier = specifier.to_utf8_lossy()?;

    // The referrer is the receiver meta object's own `url` data property, the
    // same value `import.meta.url` exposes.
    let Some(reference) = receiver.heap_reference() else {
        return import_meta_resolve_type_error(
            realm,
            origin,
            "import.meta.resolve called on an incompatible receiver",
        );
    };
    let url_key = runtime.property_key_from_string(&JsString::from_utf8("url")?)?;
    let referrer = {
        let record = runtime.object_record(reference)?;
        match record.own_property(&url_key) {
            Some(OwnProperty::Data {
                value: StoredValue::String(url),
                ..
            }) => url.to_utf8_lossy()?,
            _ => {
                return import_meta_resolve_type_error(
                    realm,
                    origin,
                    "import.meta.resolve called on an incompatible receiver",
                );
            }
        }
    };

    let resolved = match runtime.import_meta_hook() {
        Some(hook) => hook.resolve(&specifier, &referrer),
        None => Ok(crate::runtime::default_import_meta_resolve(
            &specifier, &referrer,
        )),
    };
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(error) => {
            return import_meta_resolve_type_error(realm, origin, error.message());
        }
    };
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_utf8(&resolved)?,
    )))
}

fn import_meta_resolve_type_error(
    realm: RealmId,
    origin: JsStackFrame,
    message: &str,
) -> Result<NativeDispatch, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::TypeError,
            message: JsString::from_utf8(message)?,
        },
        origin,
    }))
}
