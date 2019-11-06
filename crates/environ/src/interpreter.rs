//! Support for compiling with Lightbeam.

use crate::compilation::{Compilation, CompileError, CompiledFunction, Relocations, Traps};
use crate::func_environ::FuncEnvironment;
use crate::module::Module;
use crate::module_environ::FunctionBodyData;
// TODO: Put this in `compilation`
use crate::address_map::{ModuleAddressMap, ValueLabelsRanges};
//use crate::cranelift::RelocSink;
use cranelift_codegen::Context;
use cranelift_codegen::{binemit, ir, isa};
use cranelift_entity::{EntityRef, PrimaryMap, SecondaryMap};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_wasm::{DefinedFuncIndex, ModuleTranslationState};
use std::cmp;

fn generate_trampoline(
    isa: &dyn isa::TargetIsa,
    env: &mut FuncEnvironment,
    module: &Module,
    index: DefinedFuncIndex,
    body: Vec<u8>,
) -> Vec<u8> {
    use cranelift_codegen::ir::{InstBuilder, StackSlotData, StackSlotKind, TrapCode};

    let pointer_type = isa.pointer_type();

    let mut fn_builder_ctx = FunctionBuilderContext::new();

    let func_index = module.func_index(index);
    let call_id = func_index.index() as u32;
    let signature = &module.signatures[module.functions[func_index]];

    let values_vec_len = 8 * cmp::max(signature.params.len() - 1, signature.returns.len()) as u32;

    let mut context = Context::new();
    context.func =
        ir::Function::with_name_signature(ir::ExternalName::user(0, call_id), signature.clone());

    let ss = context.func.create_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        values_vec_len,
    ));
    let value_size = 8;

    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut fn_builder_ctx);
        let block0 = builder.create_ebb();

        builder.append_ebb_params_for_function_params(block0);
        builder.switch_to_block(block0);

        let values_vec_ptr_val = builder.ins().stack_addr(pointer_type, ss, 0);
        let mflags = ir::MemFlags::trusted();
        for i in 1..signature.params.len() {
            if i == 0 {
                continue;
            }

            let val = builder.func.dfg.ebb_params(block0)[i];
            builder.ins().store(
                mflags,
                val,
                values_vec_ptr_val,
                ((i - 1) * value_size) as i32,
            );
        }

        let eval_result = env
            .ins_eval_call(builder.cursor(), func_index, values_vec_ptr_val, body)
            .expect("translated eval");

        builder.ins().trapnz(eval_result, TrapCode::User(0));

        let mflags = ir::MemFlags::trusted();
        let mut results = Vec::new();
        for (i, r) in signature.returns.iter().enumerate() {
            let load = builder.ins().load(
                r.value_type,
                mflags,
                values_vec_ptr_val,
                (i * value_size) as i32,
            );
            results.push(load);
        }
        builder.ins().return_(&results);

        builder.seal_all_blocks();
        builder.finalize();
    }

    let mut code_buf: Vec<u8> = Vec::new();
    let mut reloc_sink = RelocSink {};
    let mut trap_sink = binemit::NullTrapSink {};
    let mut stackmap_sink = binemit::NullStackmapSink {};
    context
        .compile_and_emit(
            isa,
            &mut code_buf,
            &mut reloc_sink,
            &mut trap_sink,
            &mut stackmap_sink,
        )
        .expect("compile_and_emit");

    code_buf
}

/// A "compiler" that does not compile a WebAssembly module.
pub struct Interpreter;

impl crate::compilation::Compiler for Interpreter {
    fn compile_module<'data, 'module>(
        module: &'module Module,
        _module_translation: &ModuleTranslationState,
        function_body_inputs: PrimaryMap<DefinedFuncIndex, FunctionBodyData<'data>>,
        isa: &dyn isa::TargetIsa,
        // TODO
        generate_debug_info: bool,
    ) -> Result<
        (
            Compilation,
            Relocations,
            ModuleAddressMap,
            ValueLabelsRanges,
            PrimaryMap<DefinedFuncIndex, ir::StackSlots>,
            Traps,
        ),
        CompileError,
    > {
        if generate_debug_info {
            return Err(CompileError::DebugInfoNotSupported);
        }

        let mut functions = PrimaryMap::new();
        for (i, function_body) in &function_body_inputs {
            let mut env = FuncEnvironment::new(isa.frontend_config(), module);

            let body = function_body.data.to_vec();
            let trampoline = generate_trampoline(isa, &mut env, module, i, body);
            let function = CompiledFunction {
                body: trampoline,
                jt_offsets: SecondaryMap::new(),
                unwind_info: vec![],
            };
            functions.push(function);
        }
        let compilation = Compilation::new(functions);

        Ok((
            compilation,
            PrimaryMap::new(),
            ModuleAddressMap::new(),
            ValueLabelsRanges::new(),
            PrimaryMap::new(),
            Traps::new(),
        ))
    }
}

/// We don't expect trampoline compilation to produce any relocations, so
/// this `RelocSink` just asserts that it doesn't recieve any.
struct RelocSink {}

impl binemit::RelocSink for RelocSink {
    fn reloc_ebb(
        &mut self,
        _offset: binemit::CodeOffset,
        _reloc: binemit::Reloc,
        _ebb_offset: binemit::CodeOffset,
    ) {
        panic!("trampoline compilation should not produce ebb relocs");
    }
    fn reloc_external(
        &mut self,
        _offset: binemit::CodeOffset,
        _reloc: binemit::Reloc,
        _name: &ir::ExternalName,
        _addend: binemit::Addend,
    ) {
        panic!("trampoline compilation should not produce external symbol relocs");
    }
    fn reloc_constant(
        &mut self,
        _code_offset: binemit::CodeOffset,
        _reloc: binemit::Reloc,
        _constant_offset: ir::ConstantOffset,
    ) {
        panic!("trampoline compilation should not produce constant relocs");
    }
    fn reloc_jt(
        &mut self,
        _offset: binemit::CodeOffset,
        _reloc: binemit::Reloc,
        _jt: ir::JumpTable,
    ) {
        panic!("trampoline compilation should not produce jump table relocs");
    }
}
