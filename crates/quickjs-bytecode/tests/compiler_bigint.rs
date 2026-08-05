use std::sync::Arc;

use quickjs_bytecode::{CompilerBigInt, CompilerBigIntError, CompilerString};

fn decimal(text: &str) -> CompilerString {
    let code_units: Arc<[u16]> = text.encode_utf16().collect::<Vec<_>>().into();
    CompilerString::try_from_code_units(code_units).expect("small decimal compiler string")
}

#[test]
fn compiler_bigint_accepts_canonical_unsigned_decimal_payloads() {
    for text in ["0", "1", "18446744073709551616"] {
        let value = CompilerBigInt::try_from_decimal(decimal(text)).expect("canonical decimal");
        assert_eq!(value.decimal().latin1_units(), Some(text.as_bytes()));
        assert_eq!(value.payload_bytes(), text.len());
    }
}

#[test]
fn compiler_bigint_rejects_noncanonical_payloads() {
    assert_eq!(
        CompilerBigInt::try_from_decimal(decimal("")),
        Err(CompilerBigIntError::Empty)
    );
    assert_eq!(
        CompilerBigInt::try_from_decimal(decimal("00")),
        Err(CompilerBigIntError::LeadingZero)
    );
    assert_eq!(
        CompilerBigInt::try_from_decimal(decimal("-1")),
        Err(CompilerBigIntError::InvalidDigit {
            index: 0,
            code_unit: u16::from(b'-'),
        })
    );
    assert_eq!(
        CompilerBigInt::try_from_decimal(decimal("1_0")),
        Err(CompilerBigIntError::InvalidDigit {
            index: 1,
            code_unit: u16::from(b'_'),
        })
    );
}
