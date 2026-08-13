use std::sync::Arc;

use fusor_bytecode::{
    Binary64Constant, BytecodeBuilder, CompilerCaptureLayout, CompilerConstant,
    CompilerConstantKind, CompilerConstantLayout, CompilerConstantValue, FinalOpcode,
    FunctionGraphVerificationErrorKind, FunctionGraphVerificationLimits, FunctionIndexDomains,
    FunctionTemplateId, Operands, UnverifiedCompilerFunction, UnverifiedCompilerFunctionBody,
    UnverifiedCompilerFunctionGraph, UnverifiedFunctionHeader, VerificationLimits,
    VerifiedControlFlow, verify_compiler_control_flow, verify_compiler_function_graph,
};

fn encode(instructions: &[(FinalOpcode, Operands)]) -> Vec<u8> {
    let mut builder = BytecodeBuilder::new();
    for &(opcode, operands) in instructions {
        builder
            .push(opcode, operands)
            .expect("test instruction must encode");
    }
    builder.into_bytes()
}

fn compiler_flow(
    instructions: &[(FinalOpcode, Operands)],
    constant_kinds: &[CompilerConstantKind],
) -> Arc<VerifiedControlFlow> {
    Arc::new(
        verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                encode(instructions),
                FunctionIndexDomains::new(
                    0,
                    u32::try_from(constant_kinds.len()).expect("fixture count fits u32"),
                    0,
                    0,
                    0,
                ),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            )
            .with_capture_layout(CompilerCaptureLayout::default())
            .with_constant_layout(CompilerConstantLayout::new(Arc::from(constant_kinds))),
            VerificationLimits::default(),
        )
        .expect("fixture control flow must verify"),
    )
}

fn function(
    control_flow: Arc<VerifiedControlFlow>,
    constants: &[CompilerConstant],
) -> UnverifiedCompilerFunction {
    UnverifiedCompilerFunction::new(control_flow, Arc::from(constants), Arc::from([]))
}

#[test]
fn binary64_constants_preserve_observable_bits_and_canonicalize_nan() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Binary64Constant>();
    assert_send_sync::<CompilerConstantValue>();
    assert_send_sync::<CompilerConstant>();

    for bits in [
        0_u64,
        0x8000_0000_0000_0000,
        0x0000_0000_0000_0001,
        1.5_f64.to_bits(),
        f64::INFINITY.to_bits(),
        f64::NEG_INFINITY.to_bits(),
    ] {
        let constant = Binary64Constant::from_bits(bits);
        assert_eq!(constant.to_bits(), bits);
        assert_eq!(constant.to_f64().to_bits(), bits);
    }

    for nan_bits in [
        0x7ff0_0000_0000_0001,
        0x7ff8_0000_0000_0001,
        0xfff0_0000_0000_0001,
        0xffff_ffff_ffff_ffff,
    ] {
        assert_eq!(
            Binary64Constant::from_bits(nan_bits).to_bits(),
            Binary64Constant::CANONICAL_NAN_BITS
        );
    }
}

#[test]
fn binary64_constants_use_exact_javascript_property_name_spelling() {
    for (value, expected) in [
        (0.0, "0"),
        (-0.0, "0"),
        (1.5, "1.5"),
        (1.0e-7, "1e-7"),
        (1.0e-6, "0.000001"),
        (1.0e20, "100000000000000000000"),
        (1.0e21, "1e+21"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
        (0.000_001_234_567_890_123, "0.000001234567890123"),
    ] {
        assert_eq!(
            Binary64Constant::from_f64(value).to_javascript_string(),
            expected
        );
    }
}

#[test]
fn graph_verifier_retains_heterogeneous_value_and_function_constants() {
    let one = CompilerConstant::Value(CompilerConstantValue::Number(Binary64Constant::from_f64(
        1.5,
    )));
    let child = CompilerConstant::Function(FunctionTemplateId::new(1));
    let negative_zero = CompilerConstant::Value(CompilerConstantValue::Number(
        Binary64Constant::from_f64(-0.0),
    ));
    let root = function(
        compiler_flow(
            &[
                (FinalOpcode::PushConst8, Operands::Const8(0)),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::FClosure8, Operands::Const8(1)),
                (FinalOpcode::Drop, Operands::None),
                (FinalOpcode::PushConst8, Operands::Const8(2)),
                (FinalOpcode::Return, Operands::None),
            ],
            &[
                CompilerConstantKind::Value,
                CompilerConstantKind::Function,
                CompilerConstantKind::Value,
            ],
        ),
        &[one.clone(), child.clone(), negative_zero.clone()],
    );
    let leaf = function(
        compiler_flow(&[(FinalOpcode::ReturnUndef, Operands::None)], &[]),
        &[],
    );

    let graph = verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(FunctionTemplateId::new(0), Arc::from([root, leaf])),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("heterogeneous constants have complete owned payloads");

    assert_eq!(graph.root().constants(), [one, child, negative_zero]);
    assert_eq!(graph.max_nesting_depth(), 2);
    assert_eq!(graph.usage().constants(), 3);
}

#[test]
fn graph_verifier_rejects_declared_and_owned_constant_kind_mismatch() {
    let value = CompilerConstant::Value(CompilerConstantValue::Number(Binary64Constant::from_f64(
        1.5,
    )));
    let root = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            &[CompilerConstantKind::Function],
        ),
        std::slice::from_ref(&value),
    );

    let error = verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(FunctionTemplateId::new(0), Arc::from([root])),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("actual payload kind must match the body certificate");

    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::ConstantKindMismatch {
            index: 0,
            declared: CompilerConstantKind::Function,
            actual: CompilerConstantKind::Value,
        }
    );

    let function_payload = CompilerConstant::Function(FunctionTemplateId::new(0));
    let root = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            &[CompilerConstantKind::Value],
        ),
        &[function_payload],
    );
    let error = verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(FunctionTemplateId::new(0), Arc::from([root])),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("a function payload cannot satisfy a value-kind slot");
    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::ConstantKindMismatch {
            index: 0,
            declared: CompilerConstantKind::Value,
            actual: CompilerConstantKind::Function,
        }
    );
}

#[test]
fn function_target_errors_retain_the_heterogeneous_pool_index() {
    let value = CompilerConstant::Value(CompilerConstantValue::Number(Binary64Constant::from_f64(
        1.5,
    )));
    let target = CompilerConstant::Function(FunctionTemplateId::new(9));
    let root = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            &[CompilerConstantKind::Value, CompilerConstantKind::Function],
        ),
        &[value, target],
    );

    let error = verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(FunctionTemplateId::new(0), Arc::from([root])),
        FunctionGraphVerificationLimits::default(),
    )
    .expect_err("function targets remain checked after skipping value entries");

    assert_eq!(
        error.kind(),
        &FunctionGraphVerificationErrorKind::FunctionConstantOutOfBounds {
            index: 1,
            target: FunctionTemplateId::new(9),
            functions: 1,
        }
    );
}

#[test]
fn a_value_only_pool_does_not_create_topology_edges() {
    let value = CompilerConstant::Value(CompilerConstantValue::Number(Binary64Constant::from_f64(
        1.5,
    )));
    let root = function(
        compiler_flow(
            &[(FinalOpcode::ReturnUndef, Operands::None)],
            &[CompilerConstantKind::Value],
        ),
        std::slice::from_ref(&value),
    );

    let graph = verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(FunctionTemplateId::new(0), Arc::from([root])),
        FunctionGraphVerificationLimits::default(),
    )
    .expect("value constants do not create self-edges");

    assert_eq!(graph.root().constants(), [value]);
    assert_eq!(graph.max_nesting_depth(), 1);
    assert_eq!(graph.usage().constants(), 1);
}
