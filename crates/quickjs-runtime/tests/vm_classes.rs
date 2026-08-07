use std::sync::Arc;

use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    ExceptionKind, ExecutionError, ExecutionLimits, JsNumber, Runtime, RuntimeLimits,
};

fn compile(source: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("runtime-class.js"))
                    .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("run"))
                .expect("root function");
            let tree = context
                .compile_tree(&root, quickjs_bytecode::VerificationLimits::default())
                .expect("class bytecode");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn run_with<T>(
    source: &str,
    project: impl FnOnce(Result<quickjs_runtime::JsValue, ExecutionError>) -> T,
) -> T {
    let authority = compile(source);
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("run function");
    project(context.call(&function, &[], ExecutionLimits::default()))
}

#[test]
fn base_class_construction_public_methods_and_accessors_obey_the_class_topology() {
    run_with(
        "function run(){class Box{constructor(value){this.value=value;}get doubled(){return this.value*2;}static answer(){return 7;}}let box=new Box(5);return box.doubled+Box.answer();}",
        |result| {
            let value = result.expect("class execution");
            let number = value.as_number().expect("live value").expect("number");
            assert!(number.strict_equals(JsNumber::from_i32(17)));
        },
    );
}

#[test]
fn a_class_constructor_rejects_direct_calls_but_remains_constructable() {
    run_with(
        "function run(){class Box{constructor(){}}Box();}",
        |result| {
            let error = result.expect_err("class direct call");
            let ExecutionError::Exception(exception) = error else {
                panic!("expected JavaScript exception");
            };
            assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
        },
    );
}

#[test]
fn named_class_members_retain_the_inner_name_after_outer_reassignment() {
    run_with(
        "function run(){class Box{constructor(){}static self(){return Box;}}let original=Box;Box=0;return original.self()===original;}",
        |result| {
            let value = result.expect("class execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn a_base_class_without_a_constructor_still_constructs_with_its_class_prototype() {
    run_with(
        "function run(){class Box{static answer(){return 7;}}let box=new Box(1,2,3);return box.constructor===Box&&Box.answer()===7;}",
        |result| {
            let value = result.expect("default class construction");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn a_default_derived_constructor_forwards_arguments_and_installs_both_inheritance_links() {
    run_with(
        "function run(){class Base{constructor(value){this.value=value;}}class Derived extends Base{static answer(){return 7;}}let instance=new Derived(9);return instance.value===9&&instance.constructor===Derived&&Derived.answer()===7;}",
        |result| {
            let value = result.expect("derived class execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn initializer_free_public_instance_fields_define_on_each_receiver_at_constructor_entry() {
    run_with(
        "function run(){class Empty{empty;}class Base{base;constructor(){this.baseBeforeBody=this.base===void 0;}}class Explicit extends Base{own;constructor(){super();this.ownBeforeBody=this.own===void 0;}}class Default extends Base{forward;}let empty=new Empty;let base=new Base;let explicit=new Explicit;let forwarded=new Default;let fields=empty.empty===void 0&&base.base===void 0&&base.baseBeforeBody&&explicit.base===void 0&&explicit.own===void 0&&explicit.baseBeforeBody&&explicit.ownBeforeBody&&forwarded.base===void 0&&forwarded.forward===void 0;let descriptors=empty.hasOwnProperty('empty')&&empty.propertyIsEnumerable('empty')&&delete empty.empty&&!empty.hasOwnProperty('empty')&&base.hasOwnProperty('base')&&base.propertyIsEnumerable('base')&&delete base.base&&!base.hasOwnProperty('base')&&explicit.hasOwnProperty('own')&&explicit.propertyIsEnumerable('own')&&delete explicit.own&&!explicit.hasOwnProperty('own')&&forwarded.hasOwnProperty('forward')&&forwarded.propertyIsEnumerable('forward');return fields&&descriptors;}",
        |result| {
            let value = result.expect("instance field execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn private_instance_fields_have_fresh_class_identities_and_bypass_public_properties() {
    run_with(
        "function run(){class First{#value=1;read(){return this.#value;}write(next){return this.#value=next;}}class Second{#value=2;read(){return this.#value;}}let first=new First;let second=new Second;let own=first.hasOwnProperty('#value')===false;let values=first.read()===1&&first.write(4)===4&&first.read()===4&&second.read()===2;let rejected=false;try{First.prototype.read.call(second);}catch(error){rejected=error.name==='TypeError';}return own&&values&&rejected;}",
        |result| {
            let value = result.expect("private instance field execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn static_private_fields_are_class_owned_and_captured_by_static_methods() {
    run_with(
        "function run(){class First{static #value=1;static #named=function(){};static read(){return this.#value;}static write(next){return this.#value=next;}static has(candidate){return #value in candidate;}static name(){return this.#named.name;}}class Second{static #value=2;}let invisible=First.hasOwnProperty('#value')===false;let values=First.read()===1&&First.write(4)===4&&First.read()===4&&First.name()==='#named';let rejected=false;try{First.read.call(Second);}catch(error){rejected=error.name==='TypeError';}return invisible&&values&&First.has(First)&&!First.has(Second)&&!First.has({})&&rejected;}",
        |result| {
            let value = result.expect("static private field execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn static_private_methods_are_class_owned_immutable_and_use_the_static_home_object() {
    run_with(
        "function run(){class Base{static value(){return 'base';}}class First extends Base{static #method(){return super.value()+':private';}static call(){return this.#method();}static get(){return this.#method;}static has(candidate){return #method in candidate;}static assign(){this.#method=0;}}class Second{}let rejected=false;let immutable=false;try{First.call.call(Second);}catch(error){rejected=error.name==='TypeError';}try{First.assign();}catch(error){immutable=error.name==='TypeError';}return First.call()==='base:private'&&First.get().name==='#method'&&First.has(First)&&!First.has(Second)&&rejected&&immutable;}",
        |result| {
            let value = result.expect("static private method execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn private_accessors_merge_by_name_and_preserve_instance_static_and_super_receivers() {
    run_with(
        "function run(){class Base{value(){return 'base';}static value(){return 'static';}}class Box extends Base{#seen=0;get #value(){return super.value()+':'+this.#seen;}set #value(next){this.#seen=next;}read(){return this.#value;}write(next){this.#value=next;return this.#value;}static has(candidate){return #value in candidate;}}class First extends Base{static #seen=0;static get #value(){return super.value()+':'+this.#seen;}static set #value(next){this.#seen=next;}static read(){return this.#value;}static write(next){this.#value=next;return this.#value;}static has(candidate){return #value in candidate;}}class Readonly{get #value(){return 1;}write(){this.#value=2;}}class Writeonly{set #value(next){}read(){return this.#value;}}let box=new Box;let receiverRejected=false;let setterRejected=false;let getterRejected=false;try{Box.prototype.read.call({});}catch(error){receiverRejected=error.name==='TypeError';}try{new Readonly().write();}catch(error){setterRejected=error.name==='TypeError';}try{new Writeonly().read();}catch(error){getterRejected=error.name==='TypeError';}return box.read()==='base:0'&&box.write(4)==='base:4'&&Box.has(box)&&!Box.has({})&&First.read()==='static:0'&&First.write(7)==='static:7'&&First.has(First)&&!First.has(Box)&&receiverRejected&&setterRejected&&getterRejected;}",
        |result| {
            let value = result.expect("private accessor execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn nested_instance_field_classes_keep_private_accessor_names_lexically_distinct() {
    run_with(
        "function run(){class Outer{get #value(){}Inner=class{set #value(next){}read(){return this.#value;}};}class Other{set #value(next){}Inner=class{get #value(){}};read(){return this.#value;}}let innerRejected=false;let outerRejected=false;try{new (new Outer().Inner)().read();}catch(error){innerRejected=error.name==='TypeError';}try{new Other().read();}catch(error){outerRejected=error.name==='TypeError';}return innerRejected&&outerRejected;}",
        |result| {
            let value = result.expect("nested private accessor execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn private_instance_methods_share_a_closure_and_preserve_the_super_home_object() {
    run_with(
        "function run(){class Base{value(){return 40;}}class Box extends Base{#method(){return super.value()+2;}call(){return this.#method();}same(other){return this.#method===other.#method;}name(){return this.#method.name;}static has(candidate){return #method in candidate;}}let first=new Box;let second=new Box;let rejected=false;try{Box.prototype.call.call({});}catch(error){rejected=error.name==='TypeError';}return first.call()===42&&first.same(second)&&first.name()==='#method'&&Box.has(first)&&!Box.has({})&&rejected;}",
        |result| {
            let value = result.expect("private instance method execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn private_in_uses_private_identity_and_requires_an_object() {
    run_with(
        "function run(){class First{#value=1;static has(candidate){return #value in candidate;}}class Second{#value=2;}let first=new First;let second=new Second;let primitive=false;try{First.has(null);}catch(error){primitive=error.name==='TypeError';}return First.has(first)&&!First.has(second)&&!First.has({})&&primitive;}",
        |result| {
            let value = result.expect("private in execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn uncomputed_public_instance_field_initializers_run_at_constructor_boundaries() {
    run_with(
        "function run(){let seed=7;class Base{value=seed;copy=this.value;constructor(){this.baseBeforeBody=this.copy;}}class Explicit extends Base{next=seed+1;constructor(){super();this.derivedBeforeBody=this.next;}}class Default extends Base{forward=seed+2;}let base=new Base;let explicit=new Explicit;let forwarded=new Default;return base.value===7&&base.copy===7&&base.baseBeforeBody===7&&explicit.value===7&&explicit.copy===7&&explicit.next===8&&explicit.baseBeforeBody===7&&explicit.derivedBeforeBody===8&&forwarded.value===7&&forwarded.copy===7&&forwarded.forward===9&&forwarded.baseBeforeBody===7;}",
        |result| {
            let value = result.expect("instance field initializer execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn computed_instance_field_keys_evaluate_once_per_class_and_are_retained_by_constructors() {
    run_with(
        "function run(){let evaluations=0;function key(){evaluations=evaluations+1;return evaluations===1?'base':'derived';}class Base{[key()]=10;constructor(){this.baseSaw=evaluations;}}class Derived extends Base{[key()]=20;constructor(){super();this.derivedSaw=evaluations;}}let first=new Derived;let second=new Derived;return evaluations===2&&first.base===10&&first.derived===20&&first.baseSaw===2&&first.derivedSaw===2&&second.base===10&&second.derived===20&&second.baseSaw===2&&second.derivedSaw===2;}",
        |result| {
            let value = result.expect("computed instance field execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn computed_instance_field_keys_work_in_a_synthesized_derived_constructor() {
    run_with(
        "function run(){let evaluations=0;function key(){evaluations=evaluations+1;return 'field';}class Base{[key()]=1;}class Derived extends Base{[key()]=2;}let first=new Derived;let second=new Derived;return evaluations===2&&first.field===2&&second.field===2;}",
        |result| {
            let value = result.expect("default derived computed instance field execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn multiple_computed_instance_field_keys_keep_their_own_captured_cells() {
    run_with(
        "function run(){let calls=0;function key(){calls=calls+1;return calls===1?'left':'right';}class Box{[key()]=1;[key()]=2;}let first=new Box;let second=new Box;return calls===2&&first.left===1&&first.right===2&&second.left===1&&second.right===2;}",
        |result| {
            let value = result.expect("multiple computed instance field execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn computed_instance_field_keys_follow_class_element_evaluation_order() {
    run_with(
        "function run(){let order='';function key(label){order=order+label;return label;}class Box{[key('first')]=1;static[key('static')]=2;[key('last')]=3;}let value=new Box;return order==='firststaticlast'&&Box.static===2&&value.first===1&&value.last===3;}",
        |result| {
            let value = result.expect("computed class element ordering");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn uncomputed_instance_field_initializers_observe_this_super_and_new_target() {
    run_with(
        "function run(){class Base{constructor(value){this._value=value;}get value(){return this._value;}}class Derived extends Base{fromSuper=super.value;target=new.target;constructor(value){super(value);this.bodySeesFields=this.fromSuper===value&&this.target===Derived;}}let value=new Derived(7);return value.fromSuper===7&&value.target===Derived&&value.bodySeesFields;}",
        |result| {
            let value = result.expect("this, super, and new.target field execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn uncomputed_instance_field_initializers_create_closures_in_the_constructor_environment() {
    run_with(
        "function run(){let seed=2;class Default{value=3;read=()=>this.value+seed;readFunction=function(){return this.value+seed;};}class Explicit{value=4;read=()=>this.value+seed;constructor(){}}class Parent{value=5;}class Derived extends Parent{read=()=>this.value+seed;selected=seed?this.value+seed:0;}let defaultBox=new Default;let explicitBox=new Explicit;let derivedBox=new Derived;return defaultBox.read()===5&&defaultBox.read.name==='read'&&defaultBox.readFunction()===5&&defaultBox.readFunction.name==='readFunction'&&explicitBox.read()===6&&explicitBox.read.name==='read'&&derivedBox.read()===7&&derivedBox.read.name==='read'&&derivedBox.selected===7;}",
        |result| {
            let value = result.expect("field initializer closure execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn an_explicit_derived_constructor_calls_super_with_new_target_and_initializes_this() {
    run_with(
        "function run(){class Base{constructor(value){this.value=value;}}class Derived extends Base{constructor(value){let receiver=super(value+1);this.superReceiver=receiver===this;this.after=2;}}let instance=new Derived(4);return instance.value===5&&instance.superReceiver&&instance.after===2;}",
        |result| {
            let value = result.expect("explicit derived class execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn an_explicit_derived_constructor_enforces_the_uninitialized_this_rules() {
    run_with(
        "function run(){class Base{}class Missing extends Base{constructor(){}}class Early extends Base{constructor(){this.value=1;super();}}class Twice extends Base{constructor(){super();super();}}class ObjectReturn extends Base{constructor(){return {marked:true};}}class PrimitiveBefore extends Base{constructor(){return 1;}}class PrimitiveAfter extends Base{constructor(){super();return 1;}}let missing=false;let early=false;let twice=false;let primitiveBefore=false;let primitiveAfter=false;try{new Missing();}catch(error){missing=error.name==='ReferenceError';}try{new Early();}catch(error){early=error.name==='ReferenceError';}try{new Twice();}catch(error){twice=error.name==='ReferenceError';}try{new PrimitiveBefore();}catch(error){primitiveBefore=error.name==='TypeError';}try{new PrimitiveAfter();}catch(error){primitiveAfter=error.name==='TypeError';}return missing&&early&&twice&&primitiveBefore&&primitiveAfter&&new ObjectReturn().marked===true;}",
        |result| {
            let value = result.expect("derived constructor errors are catchable");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn class_super_properties_keep_the_method_receiver_for_reads_calls_and_writes() {
    run_with(
        "function run(){class Base{get value(){return this._value;}set value(next){this._value=next;}method(){return this._value+1;}static get answer(){return this._answer;}static set answer(next){this._answer=next;}static method(){return this._answer+1;}}class Derived extends Base{constructor(value){super();this._value=value;this.constructorRead=super.value;}read(){return super.value;}readComputed(){return super['value'];}call(){return super.method();}callComputed(){return super['method']();}write(next){return super.value=next;}writeComputed(next){return super['value']=next;}static read(){return super.answer;}static readComputed(){return super['answer'];}static call(){return super.method();}static callComputed(){return super['method']();}static write(next){return super.answer=next;}static writeComputed(next){return super['answer']=next;}}let value=new Derived(3);Derived._answer=11;return value.constructorRead===3&&value.read()===3&&value.readComputed()===3&&value.call()===4&&value.callComputed()===4&&value.write(7)===7&&value._value===7&&value.writeComputed(9)===9&&value._value===9&&Derived.read()===11&&Derived.readComputed()===11&&Derived.call()===12&&Derived.callComputed()===12&&Derived.write(13)===13&&Derived._answer===13&&Derived.writeComputed(17)===17&&Derived._answer===17;}",
        |result| {
            let value = result.expect("super property execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn static_field_super_properties_use_the_class_receiver_for_all_reference_forms() {
    run_with(
        "function run(){class Base{static get value(){return this._value;}static set value(next){this._value=next;}static method(delta){return this._value+delta;}}Base._value=3;let key='value';class Derived extends Base{static direct=super.value;static computed=super[key];static arrow=(()=>super.value)();static called=super.method(2);static assigned=(super.value=7);static compound=(super.value+=1);static logical=(super.value||=9);static updated=super.value++;}return Derived.direct===3&&Derived.computed===3&&Derived.arrow===3&&Derived.called===5&&Derived.assigned===7&&Derived.compound===8&&Derived.logical===8&&Derived.updated===8&&Base._value===3&&Derived._value===9;}",
        |result| {
            let value = result.expect("static field super property execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn static_blocks_run_in_element_order_with_lexical_class_bindings() {
    run_with(
        "function run(){let events=[];class Base{static value=2;static method(next){return this.value+next;}}class Derived extends Base{static first=(events.push('field-1'),this.value);static {events.push('block-1');let local=4;this.value=super.method(3);this.local=local;this.target=new.target;this.captured=()=>this.value;}static second=(events.push('field-2'),super.value);static {events.push('block-2');this.after=super.value+this.local;}}return Derived.first===2&&Derived.value===5&&Derived.local===4&&Derived.target===void 0&&Derived.captured()===5&&Derived.second===2&&Derived.after===6&&events.join(',')==='field-1,block-1,field-2,block-2';}",
        |result| {
            let value = result.expect("static block execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn class_super_property_compound_and_logical_assignments_keep_the_reference_once() {
    run_with(
        "function run(){class Base{get value(){return this._value;}set value(next){this._value=next;}static get answer(){return this._answer;}static set answer(next){this._answer=next;}}class Derived extends Base{constructor(value){super();this._value=value;this.keyReads=0;}key(){this.keyReads=this.keyReads+1;return 'value';}add(next){return super.value+=next;}addComputed(next){return super[this.key()]+=next;}or(next){return super.value||=next;}orComputed(next){return super[this.key()]||=next;}and(next){return super.value&&=next;}nullish(next){return super.value??=next;}pre(){return ++super.value;}post(){return super.value++;}preComputed(){return ++super[this.key()];}postComputed(){return super[this.key()]++;}static key(){this.keyReads=this.keyReads+1;return 'answer';}static add(next){return super.answer+=next;}static addComputed(next){return super[this.key()]+=next;}static or(next){return super.answer||=next;}static orComputed(next){return super[this.key()]||=next;}static and(next){return super.answer&&=next;}static nullish(next){return super.answer??=next;}static pre(){return ++super.answer;}static post(){return super.answer++;}static preComputed(){return ++super[this.key()];}static postComputed(){return super[this.key()]++;}}let value=new Derived(2);let instance=value.add(3)===5&&value._value===5;value.keyReads=0;instance=instance&&value.addComputed(4)===9&&value._value===9&&value.keyReads===1;value._value=0;instance=instance&&value.or(7)===7&&value._value===7;value._value=0;value.keyReads=0;instance=instance&&value.orComputed(8)===8&&value._value===8&&value.keyReads===1;value._value=2;instance=instance&&value.and(9)===9&&value._value===9;value._value=null;instance=instance&&value.nullish(10)===10&&value._value===10;value._value=1;instance=instance&&value.or(11)===1&&value._value===1;value._value=0;instance=instance&&value.and(12)===0&&value._value===0;value._value=2;instance=instance&&value.nullish(13)===2&&value._value===2;value._value=2;instance=instance&&value.pre()===3&&value._value===3;value._value=3;instance=instance&&value.post()===3&&value._value===4;value._value=4;value.keyReads=0;instance=instance&&value.preComputed()===5&&value._value===5&&value.keyReads===1;value._value=5;value.keyReads=0;instance=instance&&value.postComputed()===5&&value._value===6&&value.keyReads===1;Derived._answer=2;Derived.keyReads=0;let statics=Derived.add(3)===5&&Derived._answer===5&&Derived.addComputed(4)===9&&Derived._answer===9&&Derived.keyReads===1;Derived._answer=0;statics=statics&&Derived.or(7)===7&&Derived._answer===7;Derived._answer=0;Derived.keyReads=0;statics=statics&&Derived.orComputed(8)===8&&Derived._answer===8&&Derived.keyReads===1;Derived._answer=2;statics=statics&&Derived.and(9)===9&&Derived._answer===9;Derived._answer=null;statics=statics&&Derived.nullish(10)===10&&Derived._answer===10;Derived._answer=1;statics=statics&&Derived.or(11)===1&&Derived._answer===1;Derived._answer=0;statics=statics&&Derived.and(12)===0&&Derived._answer===0;Derived._answer=2;statics=statics&&Derived.nullish(13)===2&&Derived._answer===2;Derived._answer=2;statics=statics&&Derived.pre()===3&&Derived._answer===3;Derived._answer=3;statics=statics&&Derived.post()===3&&Derived._answer===4;Derived._answer=4;Derived.keyReads=0;statics=statics&&Derived.preComputed()===5&&Derived._answer===5&&Derived.keyReads===1;Derived._answer=5;Derived.keyReads=0;return instance&&statics&&Derived.postComputed()===5&&Derived._answer===6&&Derived.keyReads===1;}",
        |result| {
            let value = result.expect("super compound and logical assignment execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn a_default_class_extending_null_fails_only_when_constructed() {
    run_with(
        "function run(){class Empty extends null{}try{new Empty();}catch(error){return error.name==='TypeError';}return false;}",
        |result| {
            let value = result.expect("extends null definition and construction error");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn a_named_base_class_expression_retains_its_inner_name_and_constructs() {
    run_with(
        "function run(){let Result=class Box{static self(){return Box;}};let original=Result;Result=0;return original.self()===original&&new original().constructor===original;}",
        |result| {
            let value = result.expect("named class expression execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn a_direct_anonymous_base_class_initializer_infers_its_binding_name() {
    run_with(
        "function run(){let Result=class{static answer(){return 7;}};let original=Result;Result=0;return original.name==='Result'&&original.answer()===7&&new original().constructor===original;}",
        |result| {
            let value = result.expect("anonymous class expression execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn a_parenthesized_anonymous_base_class_initializer_infers_its_binding_name() {
    run_with(
        "function run(){let Result=(class{static answer(){return 7;}});let original=Result;Result=0;return original.name==='Result'&&original.answer()===7&&new original().constructor===original;}",
        |result| {
            let value = result.expect("parenthesized anonymous class expression execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn an_anonymous_base_class_assignment_infers_its_identifier_name() {
    run_with(
        "function run(){let Result;let assigned=(Result=class{static answer(){return 7;}});return assigned===Result&&Result.name==='Result'&&Result.answer()===7&&new Result().constructor===Result;}",
        |result| {
            let value = result.expect("anonymous class assignment execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn anonymous_base_class_logical_assignments_infer_their_identifier_names() {
    run_with(
        "function run(){let Or=0;Or||=class{};let And=1;And&&=class{};let Nullish=null;Nullish??=class{};return Or.name==='Or'&&And.name==='And'&&Nullish.name==='Nullish';}",
        |result| {
            let value = result.expect("anonymous class logical assignment execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn anonymous_base_class_binding_defaults_infer_their_identifier_names() {
    run_with(
        "function run(){let [ArrayName=class{}]=[];let {value:ObjectName=class{}}={};function parameter({ParameterName=class{}}={}){return ParameterName.name;}return ArrayName.name==='ArrayName'&&ObjectName.name==='ObjectName'&&parameter()==='ParameterName';}",
        |result| {
            let value = result.expect("anonymous class binding default execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn anonymous_base_class_assignment_defaults_infer_their_identifier_names() {
    run_with(
        "function run(){let ArrayName;[ArrayName=class{}]=[];let ObjectName;({value:ObjectName=class{}}={});return ArrayName.name==='ArrayName'&&ObjectName.name==='ObjectName';}",
        |result| {
            let value = result.expect("anonymous class assignment default execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn computed_anonymous_base_classes_name_themselves_before_installing_elements() {
    run_with(
        "function run(){let key='Result';let holder={[key]:class{value(){return 3;}}};class Box{static[key]=class{static value(){return 4;}}}return holder.Result.name==='Result'&&new holder.Result().value()===3&&Box.Result.name==='Result'&&Box.Result.value()===4;}",
        |result| {
            let value = result.expect("computed class name and element execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn computed_instance_field_anonymous_classes_use_retained_keys_for_named_evaluation() {
    run_with(
        "function run(){let key='Result';class Base{}class Box{[key]=class{value(){return 3;}}}class Explicit extends Base{[key]=class{}}class Default extends Base{[key]=class{}}let box=new Box;let explicit=new Explicit;let derived=new Default;return box.Result.name==='Result'&&box.Result.prototype.value.call(box.Result.prototype)===3&&explicit.Result.name==='Result'&&derived.Result.name==='Result';}",
        |result| {
            let value = result.expect("computed instance-field class name execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn anonymous_base_class_object_properties_infer_their_static_key_name() {
    run_with(
        "function run(){let holder={Result:class{static answer(){return 7;}}};let original=holder.Result;return original.name==='Result'&&original.answer()===7&&new original().constructor===original;}",
        |result| {
            let value = result.expect("anonymous object-property class expression execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn anonymous_base_classes_assigned_to_static_properties_keep_an_empty_name() {
    run_with(
        "function run(){let holder={};let Result=holder.Result=class{value(){return 3;}static answer(){return 4;}};return Result===holder.Result&&Result.name===''&&new Result().value()===3&&Result.answer()===4;}",
        |result| {
            let value = result.expect("anonymous property-assignment class execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn anonymous_base_classes_assigned_to_computed_properties_keep_an_empty_name() {
    run_with(
        "function run(){let events=[];function field(){events.push('class');return 4;}let key={toString(){events.push('key');return 'Result';}};let holder={};let Result=holder[key]=class{value(){return 3;}static answer=field();};return Result===holder.Result&&Result.name===''&&new Result().value()===3&&Result.answer===4&&events.join(',')==='class,key';}",
        |result| {
            let value = result.expect("anonymous computed-property class execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn anonymous_base_classes_in_ordinary_expression_contexts_keep_an_empty_name() {
    run_with(
        "function run(){let values=[class{value(){return 3;}},(true?class{static answer(){return 4;}}:class{})];return values[0].name===''&&new values[0]().value()===3&&values[1].name===''&&values[1].answer()===4;}",
        |result| {
            let value = result.expect("anonymous expression-context class execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn anonymous_base_classes_in_logical_static_property_assignments_keep_an_empty_name() {
    run_with(
        "function run(){let targets=0;let reads=0;let written;let holder={and:1,andKeep:0,nullish:null,nullishKeep:1,keep:true,get or(){reads++;return 0;},set or(value){written=value;}};function target(){targets++;return holder;}let Or=target().or||=class{static answer(){return 3;}};let And=holder.and&&=class{static answer(){return 4;}};let Nullish=holder.nullish??=class{static answer(){return 5;}};let kept=holder.keep||=class{};let keptAnd=holder.andKeep&&=class{};let keptNullish=holder.nullishKeep??=class{};return targets===1&&reads===1&&written===Or&&Or.name===''&&And.name===''&&Nullish.name===''&&Or.answer()===3&&And.answer()===4&&Nullish.answer()===5&&kept===true&&keptAnd===0&&keptNullish===1;}",
        |result| {
            let value = result.expect("anonymous logical property-assignment class execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn anonymous_base_classes_in_logical_computed_property_assignments_keep_an_empty_name() {
    run_with(
        "function run(){let events=[];function field(name){events.push('class-'+name);return 3;}function key(name){return {toString(){events.push('key-'+name);return name;}};}let written;let holder={and:1,nullish:null,get or(){events.push('get-or');return 0;},set or(value){events.push('set-or');written=value;}};let Or=holder[key('or')]||=class{static answer=field('or');};let And=holder[key('and')]&&=class{static answer=field('and');};let Nullish=holder[key('nullish')]??=class{static answer=field('nullish');};return Or.name===''&&And.name===''&&Nullish.name===''&&written===Or&&Or.answer===3&&And.answer===3&&Nullish.answer===3&&events.join(',')==='key-or,get-or,class-or,key-or,set-or,key-and,class-and,key-and,key-nullish,class-nullish,key-nullish';}",
        |result| {
            let value = result.expect("anonymous logical computed-property class execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn named_class_member_writes_throw_without_mutating_the_inner_name_cell() {
    run_with(
        "function run(){class Box{static replace(){Box=0;}}try{Box.replace();}catch(error){return error.name==='TypeError'&&Box.name==='Box';}return false;}",
        |result| {
            let value = result.expect("class name assignment completion");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn computed_public_class_methods_observe_instance_and_static_targets() {
    run_with(
        "function run(){let key='instance';class Box{[key](){return 3;}static[key+'Static'](){return 7;}}return new Box()[key]()+Box[key+'Static']();}",
        |result| {
            let value = result.expect("computed class method execution");
            let number = value.as_number().expect("live Number").expect("number");
            assert!(number.strict_equals(JsNumber::from_i32(10)));
        },
    );
}

#[test]
fn computed_public_class_accessors_define_their_respective_targets() {
    run_with(
        "function run(){let key='value';class Box{get[key](){return this._value;}set[key](value){this._value=value;}static get[key+'Static'](){return 4;}}let box=new Box;box[key]=6;return box[key]+Box[key+'Static'];}",
        |result| {
            let value = result.expect("computed class accessor execution");
            let number = value.as_number().expect("live Number").expect("number");
            assert!(number.strict_equals(JsNumber::from_i32(10)));
        },
    );
}

#[test]
fn static_public_fields_evaluate_on_the_constructor() {
    run_with(
        "function run(){let seed=7;let key='computed';class Box{static answer=seed+1;static self=Box;static Nested=class{};static empty;static[key]=seed+2;static[key+'Fn']=function(){};}let declared=Box.answer===8&&Box.self===Box&&Box.Nested.name==='Nested'&&new Box.Nested().constructor===Box.Nested&&Box.empty===void 0&&Box.computed===9&&Box.computedFn.name==='computedFn'&&Box.hasOwnProperty('answer')&&Box.hasOwnProperty('empty')&&Box.hasOwnProperty('computed')&&Box.propertyIsEnumerable('answer')&&Box.propertyIsEnumerable('computed');Box.answer=9;let writable=Box.answer===9;let configurable=delete Box.answer&&!Box.hasOwnProperty('answer');return declared&&writable&&configurable;}",
        |result| {
            let value = result.expect("static class field execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn static_field_initializers_receive_the_class_and_undefined_new_target() {
    run_with(
        "function run(){class Box{static receiver=this;static target=new.target;static captured=()=>this;static capturedTarget=()=>new.target;static ordinary=function(){return this;}}return Box.receiver===Box&&Box.target===void 0&&Box.captured()===Box&&Box.capturedTarget()===void 0&&Box.ordinary()===Box;}",
        |result| {
            let value = result.expect("static class receiver execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn anonymous_class_static_field_receivers_use_the_created_constructor() {
    run_with(
        "function run(){let Box=class{static receiver=this;static captured=()=>this;};return Box.receiver===Box&&Box.captured()===Box;}",
        |result| {
            let value = result.expect("anonymous static class receiver execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}

#[test]
fn derived_super_spread_preserves_the_active_new_target_and_receiver_timing() {
    run_with(
        "function run(){let events=[];function args(){events.push('args');return [2,3];}class Base{constructor(...values){events.push('base');this.values=values.join(':');this.target=new.target;}}class Derived extends Base{constructor(){events.push('before');super(1,...args(),4);events.push('after');this.ready=true;}}class Leaf extends Derived{}let value=new Leaf;return value.values==='1:2:3:4'&&value.target===Leaf&&value.ready&&events.join(',')==='before,args,base,after';}",
        |result| {
            let value = result.expect("derived super spread execution");
            assert_eq!(value.as_boolean().expect("live Boolean"), Some(true));
        },
    );
}
