use quickjs_bytecode::{
    CompilerBindingKind, CompilerExecutableKind, FinalOpcode, VerificationLimits,
};
use quickjs_compiler::{CompilationContext, CompiledFunctionTree, DeclarationKind, WritePolicy};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("root function");
            context
                .compile_tree(&root, VerificationLimits::default())
                .expect("base class tree")
        },
    )
    .expect("frontend")
}

#[test]
fn explicit_base_class_constructor_and_public_methods_lower_to_typed_class_bytecode() {
    let tree = compile(
        "function make(){class Box{constructor(value){this.value=value;}get doubled(){return this.value*2;}static answer(){return 7;}}return Box;}",
        "make",
    );
    let root = tree.root();
    let opcodes = root
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();

    assert!(opcodes.windows(2).any(|pair| {
        matches!(
            pair,
            [
                FinalOpcode::FClosure8 | FinalOpcode::FClosure,
                FinalOpcode::DefineClass
            ]
        )
    }));
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::DefineMethod)
            .count(),
        2
    );
    assert_eq!(tree.functions().len(), 4);
    assert_eq!(
        tree.verified_bytecode()
            .function(quickjs_bytecode::FunctionTemplateId::new(1))
            .expect("class constructor")
            .metadata()
            .executable_kind(),
        CompilerExecutableKind::ClassConstructor
    );
    assert!(
        !tree.functions()[1]
            .control_flow()
            .function_header()
            .flags()
            .has_prototype(),
        "define_class owns the public prototype rather than the closure header"
    );
    assert!(
        tree.functions().iter().skip(1).all(|function| function
            .control_flow()
            .function_header()
            .mode()
            .is_strict()),
        "class constructors and methods are strict irrespective of the enclosing function"
    );
}

#[test]
fn public_private_instance_fields_receive_fresh_class_scope_names() {
    let tree = compile(
        "function make(){class Box{#value=1;read(){return this.#value;}write(next){return this.#value=next;}static has(candidate){return #value in candidate;}}return Box;}",
        "make",
    );
    let opcodes = tree
        .functions()
        .iter()
        .flat_map(|function| function.control_flow().instructions())
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();

    assert!(opcodes.contains(&FinalOpcode::PrivateSymbol));
    assert!(opcodes.contains(&FinalOpcode::DefinePrivateField));
    assert!(
        tree.functions()
            .iter()
            .flat_map(|function| function.control_flow().instructions())
            .any(|instruction| matches!(
                (
                    instruction.decoded().instruction().opcode(),
                    instruction.decoded().instruction().operands(),
                ),
                (
                    FinalOpcode::DefinePrivateField,
                    quickjs_bytecode::Operands::U8(0)
                )
            ))
    );
    assert!(opcodes.contains(&FinalOpcode::GetPrivateField));
    assert!(opcodes.contains(&FinalOpcode::PutPrivateField));
    assert!(opcodes.contains(&FinalOpcode::PrivateIn));
    assert!(tree.functions().iter().any(|function| {
        function
            .closure_variables()
            .iter()
            .any(|closure| closure.policy().kind() == DeclarationKind::ClassPrivateName)
    }));
}

#[test]
fn private_fields_after_optional_chains_lower_with_brand_checked_reads() {
    let tree = compile(
        "function make(){return class Box{#value=7;#read(){return 9;}static direct(value){return value?.#value;}static nested(value){return value?.member.#value;}static directCall(value){return value?.#read();}static optionalCall(value){return value.#read?.();}}}",
        "make",
    );
    let opcodes = tree
        .functions()
        .iter()
        .flat_map(|function| function.control_flow().instructions())
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();

    assert!(opcodes.contains(&FinalOpcode::IsUndefinedOrNull));
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::GetPrivateField)
            .count(),
        4
    );
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::CallMethod)
            .count(),
        2
    );
}

#[test]
fn private_member_writes_lower_as_single_receiver_name_references() {
    let tree = compile(
        "function make(){class Box{#value=1;compound(next){return this.#value+=next;}or(next){return this.#value||=next;}and(next){return this.#value&&=next;}nullish(next){return this.#value??=next;}pre(){return ++this.#value;}post(){return this.#value--;}}return Box;}",
        "make",
    );
    let opcodes = tree
        .functions()
        .iter()
        .flat_map(|function| function.control_flow().instructions())
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();

    assert!(opcodes.contains(&FinalOpcode::GetPrivateField));
    assert!(opcodes.contains(&FinalOpcode::PutPrivateField));
    assert!(opcodes.contains(&FinalOpcode::Dup2));
    assert!(opcodes.contains(&FinalOpcode::Insert3));
    assert!(opcodes.contains(&FinalOpcode::Perm4));
    assert!(opcodes.contains(&FinalOpcode::PostDec));
}

#[test]
fn private_instance_methods_keep_distinct_names_shared_closures_and_home_objects() {
    let tree = compile(
        "function make(){class Base{value(){return 40;}}class Box extends Base{#method(){return super.value()+2;}call(){return this.#method();}same(other){return this.#method===other.#method;}name(){return this.#method.name;}static has(candidate){return #method in candidate;}}return Box;}",
        "make",
    );
    let opcodes = tree
        .functions()
        .iter()
        .flat_map(|function| function.control_flow().instructions())
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();

    assert!(opcodes.contains(&FinalOpcode::PrivateSymbol));
    assert!(opcodes.contains(&FinalOpcode::DefinePrivateField));
    assert!(
        tree.functions()
            .iter()
            .flat_map(|function| function.control_flow().instructions())
            .any(|instruction| matches!(
                (
                    instruction.decoded().instruction().opcode(),
                    instruction.decoded().instruction().operands(),
                ),
                (
                    FinalOpcode::DefinePrivateField,
                    quickjs_bytecode::Operands::U8(1)
                )
            ))
    );
    assert!(opcodes.contains(&FinalOpcode::GetPrivateField));
    assert!(opcodes.contains(&FinalOpcode::SetHomeObject));
    assert!(opcodes.contains(&FinalOpcode::CallMethod));
    assert!(tree.functions().iter().any(|function| {
        function
            .closure_variables()
            .iter()
            .filter(|closure| closure.policy().kind() == DeclarationKind::ClassPrivateName)
            .count()
            >= 2
    }));
}

#[test]
fn private_async_and_generator_methods_keep_typed_method_templates() {
    let tree = compile(
        "function make(){class Box{*#values(){yield 1;}async #asyncValue(){return 2;}async *#asyncValues(){yield 3;}static *#staticValues(){yield 4;}}return Box;}",
        "make",
    );
    let private_method_definitions = tree
        .functions()
        .iter()
        .flat_map(|function| function.control_flow().instructions())
        .filter(|instruction| {
            matches!(
                (
                    instruction.decoded().instruction().opcode(),
                    instruction.decoded().instruction().operands(),
                ),
                (
                    FinalOpcode::DefinePrivateField,
                    quickjs_bytecode::Operands::U8(1)
                )
            )
        })
        .count();

    assert_eq!(private_method_definitions, 4);
    assert!(tree.verified_bytecode().functions().any(|function| {
        function.metadata().executable_kind() == CompilerExecutableKind::GeneratorMethod
    }));
    assert!(tree.verified_bytecode().functions().any(|function| {
        function.metadata().executable_kind() == CompilerExecutableKind::AsyncMethod
    }));
    assert!(tree.verified_bytecode().functions().any(|function| {
        function.metadata().executable_kind() == CompilerExecutableKind::AsyncGeneratorMethod
    }));
}

#[test]
fn a_base_class_without_a_constructor_uses_a_synthesized_typed_template() {
    let tree = compile(
        "function make(){class Box{static answer(){return 7;}}return Box;}",
        "make",
    );
    assert_eq!(
        tree.functions().len(),
        3,
        "one synthesized constructor and one method template"
    );
    assert_eq!(
        tree.verified_bytecode()
            .function(quickjs_bytecode::FunctionTemplateId::new(1))
            .expect("synthesized class constructor")
            .metadata()
            .executable_kind(),
        CompilerExecutableKind::ClassConstructor
    );
}

#[test]
fn explicit_derived_constructors_lower_a_typed_super_construction_path() {
    let tree = compile(
        "function make(){class Base{constructor(value){this.value=value;}}class Derived extends Base{constructor(value){super(value+1);this.after=2;}}return Derived;}",
        "make",
    );
    let (constructor_index, _) = tree
        .functions()
        .iter()
        .enumerate()
        .find(|function| {
            function
                .1
                .control_flow()
                .function_header()
                .flags()
                .is_derived_class_constructor()
        })
        .expect("derived constructor");
    let constructor = tree
        .verified_bytecode()
        .function(quickjs_bytecode::FunctionTemplateId::new(
            u32::try_from(constructor_index).expect("template index"),
        ))
        .expect("verified derived constructor");
    let instructions = constructor.function().control_flow().instructions();
    assert!(instructions.windows(3).any(|window| {
        matches!(
            window,
            [active, super_constructor, new_target]
                if matches!(
                    active.decoded().instruction().operands(),
                    quickjs_bytecode::Operands::U8(4)
                )
                    && super_constructor.decoded().instruction().opcode() == FinalOpcode::GetSuper
                    && matches!(
                        new_target.decoded().instruction().operands(),
                        quickjs_bytecode::Operands::U8(3)
                    )
        )
    }));
    assert!(instructions.windows(3).any(|window| {
        matches!(
            window,
            [call, completion, drop]
                if call.decoded().instruction().opcode() == FinalOpcode::CallConstructor
                    && completion.decoded().instruction().opcode() == FinalOpcode::CheckCtorReturn
                    && drop.decoded().instruction().opcode() == FinalOpcode::Drop
        )
    }));
}

#[test]
fn class_super_properties_lower_through_home_object_and_receiver_aware_opcodes() {
    let tree = compile(
        "function make(){class Base{get value(){return this._value;}set value(next){this._value=next;}method(){return this._value;}static get answer(){return this._answer;}static set answer(next){this._answer=next;}}class Derived extends Base{read(){return super.value;}call(){return super.method();}write(next){return super.value=next;}add(next){return super.value+=next;}assign(next){return super['value']||=next;}pre(){return ++super.value;}post(){return super['value']++;}*values(){yield super.value;}async asyncRead(){return super.value;}async *asyncValues(){yield super.value;}static read(){return super.answer;}static write(next){return super.answer=next;}static add(next){return super.answer+=next;}static assign(next){return super['answer']&&=next;}static pre(){return ++super.answer;}static post(){return super.answer++;}}return Derived;}",
        "make",
    );
    let opcodes = tree
        .functions()
        .iter()
        .flat_map(|function| function.control_flow().instructions())
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert!(opcodes.contains(&FinalOpcode::GetSuper));
    assert!(opcodes.contains(&FinalOpcode::GetSuperValue));
    assert!(opcodes.contains(&FinalOpcode::PutSuperValue));
    assert!(opcodes.contains(&FinalOpcode::Dup3));
    assert!(opcodes.contains(&FinalOpcode::Insert4));
    assert!(opcodes.contains(&FinalOpcode::Perm5));
    assert!(tree.verified_bytecode().functions().any(|function| {
        function.metadata().executable_kind() == CompilerExecutableKind::GeneratorMethod
    }));
    assert!(tree.verified_bytecode().functions().any(|function| {
        function.metadata().executable_kind() == CompilerExecutableKind::AsyncMethod
    }));
    assert!(tree.verified_bytecode().functions().any(|function| {
        function.metadata().executable_kind() == CompilerExecutableKind::AsyncGeneratorMethod
    }));
    assert!(tree.functions().iter().any(|function| {
        function
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                matches!(
                    instruction.decoded().instruction().operands(),
                    quickjs_bytecode::Operands::U8(5)
                )
            })
    }));
}

#[test]
fn a_named_base_class_expression_uses_the_same_typed_definition_path() {
    let tree = compile(
        "function make(){let Result=class Box{static self(){return Box;}};return Result;}",
        "make",
    );
    assert_eq!(tree.functions().len(), 3);
    assert_eq!(
        tree.verified_bytecode()
            .function(quickjs_bytecode::FunctionTemplateId::new(1))
            .expect("synthesized expression class constructor")
            .metadata()
            .executable_kind(),
        CompilerExecutableKind::ClassConstructor
    );
}

#[test]
fn a_direct_anonymous_base_class_initializer_uses_its_binding_name() {
    let tree = compile(
        "function make(){let Result=class{static answer(){return 7;}};return Result;}",
        "make",
    );
    assert_eq!(tree.functions().len(), 3);
    assert_eq!(
        tree.verified_bytecode()
            .function(quickjs_bytecode::FunctionTemplateId::new(1))
            .expect("synthesized anonymous class constructor")
            .metadata()
            .executable_kind(),
        CompilerExecutableKind::ClassConstructor
    );
    assert!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass),
        "the inferred name is supplied to define_class, not a post-closure SetName"
    );
    assert!(
        !tree
            .root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::SetName),
        "class inference cannot reuse the ordinary-function SetName opcode"
    );
}

#[test]
fn a_direct_anonymous_base_class_assignment_uses_its_target_name() {
    let tree = compile(
        "function make(){let Result;return Result=class{static answer(){return 7;}};}",
        "make",
    );
    assert_eq!(tree.functions().len(), 3);
    assert!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass),
        "the inferred name is supplied to define_class, not a post-closure SetName"
    );
    assert!(
        !tree
            .root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::SetName),
        "class inference cannot reuse the ordinary-function SetName opcode"
    );
}

#[test]
fn anonymous_base_class_binding_defaults_use_their_binding_names() {
    let tree = compile(
        "function make(){let [ArrayName=class{}]=[];let {value:ObjectName=class{}}={};return [ArrayName,ObjectName];}",
        "make",
    );
    assert_eq!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass)
            .count(),
        2,
        "both defaults receive their inferred name through define_class"
    );
    assert!(
        !tree
            .root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::SetName),
        "class inference cannot reuse the ordinary-function SetName opcode"
    );
}

#[test]
fn anonymous_base_class_assignment_defaults_use_their_target_names() {
    let tree = compile(
        "function make(){let ArrayName;[ArrayName=class{}]=[];let ObjectName;({value:ObjectName=class{}}={});return [ArrayName,ObjectName];}",
        "make",
    );
    assert_eq!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass)
            .count(),
        2,
        "both defaults receive their inferred name through define_class"
    );
    assert!(
        !tree
            .root()
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::SetName),
        "class inference cannot reuse the ordinary-function SetName opcode"
    );
}

#[test]
fn computed_anonymous_base_classes_use_the_typed_computed_name_path() {
    let tree = compile(
        "function make(key){let holder={[key]:class{value(){return 3;}}};class Box{static[key]=class{static value(){return 4;}}}return [holder,Box];}",
        "make",
    );
    let opcodes = tree
        .root()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::SetNameComputed)
            .count(),
        2
    );
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::DefineArrayEl)
            .count(),
        2
    );
}

#[test]
fn named_class_member_writes_retain_a_dedicated_immutable_class_name_capture() {
    let tree = compile(
        "function make(){class Box{static replace(){Box=0;}}return Box;}",
        "make",
    );
    let method = tree
        .verified_bytecode()
        .function(quickjs_bytecode::FunctionTemplateId::new(2))
        .expect("static method template");
    assert!(
        method
            .metadata()
            .closures()
            .iter()
            .any(|definition| { definition.policy().kind() == CompilerBindingKind::ClassName })
    );
}

#[test]
fn computed_public_class_methods_use_the_typed_computed_definition_path() {
    let tree = compile(
        "function make(key){class Box{[key](){return 3;}get[key+'Get'](){return 4;}set[key+'Set'](value){}static[key+'Static'](){return 7;}static get[key+'StaticGet'](){return 8;}static set[key+'StaticSet'](value){}}return Box;}",
        "make",
    );
    let flags = tree
        .root()
        .control_flow()
        .instructions()
        .iter()
        .filter_map(|instruction| {
            let instruction = instruction.decoded().instruction();
            match (instruction.opcode(), instruction.operands()) {
                (FinalOpcode::DefineMethodComputed, quickjs_bytecode::Operands::U8(flags)) => {
                    Some(flags)
                }
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(flags, [0, 1, 2, 0, 1, 2]);
}

#[test]
fn async_generator_class_methods_are_owned_by_their_definition() {
    let tree = compile(
        "function make(){class Box{async *values(){yield 1;}}return Box;}",
        "make",
    );
    assert!(tree.verified_bytecode().functions().any(|function| {
        function.metadata().executable_kind() == CompilerExecutableKind::AsyncGeneratorMethod
    }));
}

#[test]
fn static_class_fields_lower_to_the_typed_field_definition_path() {
    let tree = compile(
        "function make(seed){class Box{static answer=seed+1;static self=Box;static Nested=class{};static empty;}return Box;}",
        "make",
    );
    assert_eq!(
        tree.root()
            .control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineField)
            .count(),
        4
    );
}

#[test]
fn initializer_free_public_instance_fields_lower_into_each_constructor() {
    let tree = compile(
        "function make(){class Empty{empty;}class Base{base;constructor(){}}class Explicit extends Base{own;constructor(){super();}}class Default extends Base{forward;}return [Empty,Base,Explicit,Default];}",
        "make",
    );
    let constructors = tree
        .verified_bytecode()
        .functions()
        .filter(|function| {
            function.metadata().executable_kind() == CompilerExecutableKind::ClassConstructor
        })
        .collect::<Vec<_>>();
    assert_eq!(constructors.len(), 4);
    assert_eq!(
        tree.functions()
            .iter()
            .flat_map(|function| function.control_flow().instructions())
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineField)
            .count(),
        4,
    );
}

#[test]
fn computed_static_class_fields_use_the_typed_dynamic_definition_path() {
    let tree = compile(
        "function make(key){class Box{static[key]=1;static[key+'Fn']=function(){};}return Box;}",
        "make",
    );
    let opcodes = tree
        .root()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::DefineArrayEl)
            .count(),
        2
    );
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::SetNameComputed)
            .count(),
        1
    );
}

#[test]
fn static_property_class_assignments_use_a_typed_empty_name_atom() {
    let tree = compile(
        "function make(holder){return holder.Result=class{value(){return 3;}static answer(){return 4;}};}",
        "make",
    );
    let root = tree.root();
    assert!(
        root.control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::DefineClass
            })
    );
    assert!(
        root.atoms()
            .iter()
            .any(|atom| atom.is_static_property_only() && atom.string().is_empty())
    );
    assert!(
        !root
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::SetName
            }),
        "a property assignment leaves the anonymous class name empty"
    );
}

#[test]
fn computed_property_class_assignments_use_the_same_typed_empty_name_atom() {
    let tree = compile(
        "function make(holder,key){return holder[key]=class{value(){return 3;}static answer(){return 4;}};}",
        "make",
    );
    let root = tree.root();
    assert!(
        root.control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::DefineClass
            })
    );
    assert!(
        root.atoms()
            .iter()
            .any(|atom| atom.is_static_property_only() && atom.string().is_empty())
    );
    assert!(
        !root
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::SetName
            }),
        "a computed property assignment leaves the anonymous class name empty"
    );
}

#[test]
fn ordinary_expression_contexts_use_typed_empty_class_name_atoms() {
    let tree = compile(
        "function make(){return [class{value(){return 3;}},(true?class{static answer(){return 4;}}:class{})];}",
        "make",
    );
    let root = tree.root();
    assert_eq!(
        root.control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass)
            .count(),
        3,
    );
    assert!(
        root.atoms()
            .iter()
            .any(|atom| atom.is_static_property_only() && atom.string().is_empty())
    );
    assert!(
        !root
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::SetName
            }),
        "ordinary expression contexts retain the anonymous class default name"
    );
}

#[test]
fn logical_static_property_class_assignments_use_the_typed_empty_name_path() {
    let tree = compile(
        "function make(holder){holder.or||=class{static answer(){return 3;}};holder.and&&=class{static answer(){return 4;}};holder.nullish??=class{static answer(){return 5;}};holder.andKeep&&=class{};holder.nullishKeep??=class{};return holder;}",
        "make",
    );
    let root = tree.root();
    assert_eq!(
        root.control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass)
            .count(),
        5,
    );
    assert!(
        root.atoms()
            .iter()
            .any(|atom| atom.is_static_property_only() && atom.string().is_empty())
    );
    assert!(
        root.control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::Swap
            })
    );
}

#[test]
fn logical_computed_property_class_assignments_preserve_the_raw_key_for_read_and_write() {
    let tree = compile(
        "function make(holder,orKey,andKey,nullishKey){holder[orKey]||=class{static answer(){return 3;}};holder[andKey]&&=class{static answer(){return 4;}};holder[nullishKey]??=class{static answer(){return 5;}};return holder;}",
        "make",
    );
    let root = tree.root();
    assert_eq!(
        root.control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass)
            .count(),
        3,
    );
    assert_eq!(
        root.control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::Dup2)
            .count(),
        3,
    );
    assert_eq!(
        root.control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::GetArrayEl)
            .count(),
        3,
    );
}

#[test]
fn nonlogical_member_class_assignments_use_the_typed_empty_name_path() {
    let tree = compile(
        "function make(holder,key){holder.value+=class{};holder[key]*=class{};return holder;}",
        "make",
    );
    let root = tree.root();
    assert_eq!(
        root.control_flow()
            .instructions()
            .iter()
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineClass)
            .count(),
        2,
    );
    assert!(
        root.atoms()
            .iter()
            .any(|atom| atom.is_static_property_only() && atom.string().is_empty())
    );
    assert!(
        root.control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::Dup2
            })
    );
}

#[test]
fn static_field_initializers_use_a_dedicated_class_receiver_cell() {
    let tree = compile(
        "function make(){class Box{static receiver=this;static target=new.target;static captured=()=>this;}return Box;}",
        "make",
    );
    assert!(matches!(
        tree.root()
            .storage_plan()
            .bindings()
            .iter()
            .find(|binding| binding.policy().kind() == DeclarationKind::ClassStaticReceiver),
        Some(binding)
            if binding.placement() == quickjs_compiler::StoragePlacement::Local
                && binding.policy().has_temporal_dead_zone()
    ));
    assert!(
        tree.functions()
            .iter()
            .flat_map(|function| function.control_flow().instructions())
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::GetVarRefCheck
            })
    );
}

#[test]
fn static_field_super_uses_the_lexical_class_receiver() {
    let tree = compile(
        "function make(Base){class Box extends Base{static inherited=super.value;}return Box;}",
        "make",
    );
    let root = tree.root();
    assert!(
        tree.verified_bytecode()
            .function(quickjs_bytecode::FunctionTemplateId::new(0))
            .expect("root verified function")
            .metadata()
            .variables()
            .iter()
            .any(
                |definition| definition.policy().kind() == CompilerBindingKind::ClassStaticReceiver
            )
    );
    let instructions = root.control_flow().instructions();
    assert!(instructions.iter().any(|instruction| {
        instruction.decoded().instruction().opcode() == FinalOpcode::GetSuper
    }));
    assert!(instructions.iter().any(|instruction| {
        instruction.decoded().instruction().opcode() == FinalOpcode::GetLocCheck
    }));
    assert!(!instructions.iter().any(|instruction| {
        matches!(
            instruction.decoded().instruction().operands(),
            quickjs_bytecode::Operands::U8(5)
        )
    }));
}

#[test]
fn static_blocks_lower_in_class_element_order_with_a_lexical_receiver() {
    let tree = compile(
        "function make(Base){class Box extends Base{static first=this.value;static {let local=4;this.value=super.method(3);this.local=local;this.target=new.target;this.captured=()=>this.value;}static second=super.value;static {this.after=super.value+this.local;}}return Box;}",
        "make",
    );
    let root = tree.root();
    let instructions = root.control_flow().instructions();
    assert!(root.storage_plan().bindings().iter().any(|binding| {
        binding.policy().kind() == DeclarationKind::ClassStaticReceiver
            && binding.placement() == quickjs_compiler::StoragePlacement::Local
    }));
    assert!(instructions.iter().any(|instruction| {
        instruction.decoded().instruction().opcode() == FinalOpcode::GetSuper
    }));
    assert!(instructions.iter().any(|instruction| {
        instruction.decoded().instruction().opcode() == FinalOpcode::Undefined
    }));
    assert!(tree.functions().iter().any(|function| {
        function
            .control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::GetVarRefCheck
            })
    }));
}

#[test]
fn derived_super_spread_uses_the_typed_constructor_apply_form() {
    let tree = compile(
        "function make(Base){return class Derived extends Base{constructor(args){super(...args);}}}",
        "make",
    );
    assert!(
        tree.functions()
            .iter()
            .flat_map(|function| function.control_flow().instructions())
            .any(|instruction| {
                let instruction = instruction.decoded().instruction();
                instruction.opcode() == FinalOpcode::Apply
                    && instruction.operands() == quickjs_bytecode::Operands::U16(2)
            })
    );
}

#[test]
fn private_accessors_share_one_name_and_lower_to_accessor_kinds() {
    let tree = compile(
        "function make(){class Box{get #value(){return 1;}set #value(next){}read(){return this.#value;}write(next){this.#value=next;}}return Box;}",
        "make",
    );
    let instructions = tree
        .functions()
        .iter()
        .flat_map(|function| function.control_flow().instructions())
        .map(|instruction| instruction.decoded().instruction())
        .collect::<Vec<_>>();

    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| instruction.opcode() == FinalOpcode::PrivateSymbol)
            .count(),
        1
    );
    for kind in [2, 3] {
        assert!(instructions.iter().any(|instruction| matches!(
            (instruction.opcode(), instruction.operands()),
            (FinalOpcode::DefinePrivateField, quickjs_bytecode::Operands::U8(value)) if value == kind
        )));
    }
}

#[test]
fn uncomputed_public_instance_field_initializers_lower_into_each_constructor() {
    let tree = compile(
        "function make(seed){class Base{value=seed;constructor(){}}class Derived extends Base{next=seed+1;constructor(){super();}}class Default extends Base{forward=seed+2;}return [Base,Derived,Default];}",
        "make",
    );
    assert_eq!(
        tree.functions()
            .iter()
            .flat_map(|function| function.control_flow().instructions())
            .filter(|instruction| instruction.decoded().instruction().opcode()
                == FinalOpcode::DefineField)
            .count(),
        3,
    );
}

#[test]
fn computed_public_instance_fields_capture_a_once_evaluated_class_key() {
    let tree = compile(
        "function make(key){class Box{[key]=key;}return Box;}",
        "make",
    );
    assert!(
        tree.root()
            .storage_plan()
            .bindings()
            .iter()
            .any(|binding| { binding.policy().kind() == DeclarationKind::ClassFieldKey })
    );
    assert!(
        tree.root()
            .control_flow()
            .instructions()
            .windows(2)
            .any(|pair| {
                pair[0].decoded().instruction().opcode() == FinalOpcode::ToPropKey
                    && matches!(
                        pair[1].decoded().instruction().opcode(),
                        FinalOpcode::PutLoc
                            | FinalOpcode::PutLoc8
                            | FinalOpcode::PutLoc0
                            | FinalOpcode::PutLoc1
                            | FinalOpcode::PutLoc2
                            | FinalOpcode::PutLoc3
                    )
            })
    );
    let constructor_index = tree
        .functions()
        .iter()
        .enumerate()
        .find_map(|(index, _)| {
            (tree
                .verified_bytecode()
                .function(quickjs_bytecode::FunctionTemplateId::new(
                    u32::try_from(index).expect("template index"),
                ))
                .expect("verified function")
                .metadata()
                .executable_kind()
                == CompilerExecutableKind::ClassConstructor)
                .then_some(index)
        })
        .expect("class constructor");
    let opcodes = tree.functions()[constructor_index]
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert!(
        opcodes
            .windows(2)
            .any(|pair| { pair == [FinalOpcode::PushThis, FinalOpcode::GetVarRefCheck] })
    );
    assert!(opcodes.contains(&FinalOpcode::DefineArrayEl));
}

#[test]
fn interleaved_computed_fields_capture_all_keys_before_static_initializers() {
    let tree = compile(
        "function make(){let index=0;class Box{[index++]=index++;static[index++]=index++;[index++]=index++;}return Box;}",
        "make",
    );
    let root = tree.root();
    assert_eq!(
        root.storage_plan()
            .bindings()
            .iter()
            .filter(|binding| binding.policy().kind() == DeclarationKind::ClassFieldKey)
            .count(),
        3
    );
    let opcodes = root
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    let last_key = opcodes
        .iter()
        .rposition(|&opcode| opcode == FinalOpcode::ToPropKey)
        .expect("computed field keys");
    let first_static_initializer = opcodes
        .iter()
        .position(|&opcode| opcode == FinalOpcode::DefineArrayEl)
        .expect("computed static field initializer");
    assert!(last_key < first_static_initializer);
}

#[test]
fn named_base_class_members_capture_a_distinct_immutable_class_name_cell() {
    let tree = compile(
        "function make(){class Box{constructor(){}static self(){return Box;}}}",
        "make",
    );
    let class_bindings = tree
        .root()
        .storage_plan()
        .bindings()
        .iter()
        .filter(|binding| binding.name() == "Box")
        .collect::<Vec<_>>();
    assert_eq!(
        class_bindings.len(),
        2,
        "outer and inner class names differ"
    );
    assert!(
        class_bindings.iter().any(|binding| {
            binding.is_frame_captured() && binding.policy().writes() == WritePolicy::Immutable
        }),
        "the method must capture the immutable synthetic class-name cell"
    );
    let opcodes = tree
        .root()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert!(opcodes.contains(&FinalOpcode::CloseLoc));
}

#[test]
fn derived_class_without_an_explicit_constructor_lowers_a_certified_heritage_path() {
    let tree = compile(
        "function make(Base){return class Derived extends Base{static answer(){return 7;}}}",
        "make",
    );
    let root = tree.root();
    let opcodes = root
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();

    assert!(opcodes.windows(9).any(|window| {
        matches!(
            window,
            [
                FinalOpcode::Dup,
                FinalOpcode::IsNull,
                FinalOpcode::IfTrue | FinalOpcode::IfTrue8,
                FinalOpcode::CheckCtor,
                FinalOpcode::Dup,
                FinalOpcode::GetField,
                FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16,
                FinalOpcode::Null,
                FinalOpcode::FClosure8 | FinalOpcode::FClosure,
            ]
        )
    }));
    assert!(
        root.control_flow()
            .instructions()
            .iter()
            .any(|instruction| {
                matches!(
                    instruction.decoded().instruction().operands(),
                    quickjs_bytecode::Operands::AtomU8 { value: 1, .. }
                ) && instruction.decoded().instruction().opcode() == FinalOpcode::DefineClass
            })
    );
    let constructor = tree
        .verified_bytecode()
        .function(quickjs_bytecode::FunctionTemplateId::new(1))
        .expect("derived class constructor");
    assert!(
        constructor
            .function()
            .control_flow()
            .function_header()
            .flags()
            .is_derived_class_constructor()
    );
    assert_eq!(
        constructor
            .function()
            .control_flow()
            .instructions()
            .iter()
            .map(|instruction| instruction.decoded().instruction().opcode())
            .collect::<Vec<_>>(),
        vec![
            FinalOpcode::CheckCtor,
            FinalOpcode::InitCtor,
            FinalOpcode::Drop,
            FinalOpcode::ReturnUndef,
        ]
    );
}
