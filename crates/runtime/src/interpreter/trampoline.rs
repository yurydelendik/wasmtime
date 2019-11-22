#![allow(dead_code)]

use super::code_memory::CodeMemory;
use crate::{wasmtime_call_trampoline, VMContext, VMFunctionBody, VMInvokeArgument};
use cranelift_codegen::ir::InstBuilder;
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::{binemit, ir, isa, settings, Context};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use std::cmp::max;
use std::collections::HashMap;
use std::mem;
use std::ptr;
use wasmtime_environ::CompiledFunction;

use wasmeval::Val;

pub(crate) struct TrampolineError(pub &'static str);

fn native_isa() -> Box<dyn isa::TargetIsa> {
    let flag_builder = settings::builder();
    let isa_builder = cranelift_native::builder().unwrap_or_else(|_| {
        panic!("host machine is not a supported target");
    });
    isa_builder.finish(settings::Flags::new(flag_builder))
}

pub(crate) struct TrampolineCache {
    isa: Box<dyn TargetIsa>,
    code_memory: CodeMemory,
    trampoline_park: HashMap<*const VMFunctionBody, *const VMFunctionBody>,
    /// The `FunctionBuilderContext`, shared between trampline function compilations.
    fn_builder_ctx: FunctionBuilderContext,
}

impl TrampolineCache {
    pub fn new() -> Self {
        TrampolineCache {
            isa: native_isa(),
            code_memory: CodeMemory::new(),
            trampoline_park: HashMap::new(),
            fn_builder_ctx: FunctionBuilderContext::new(),
        }
    }

    /// Create a trampoline for invoking a function.
    pub(crate) fn get_trampoline(
        &mut self,
        callee_address: *const VMFunctionBody,
        signature: &ir::Signature,
        value_size: usize,
    ) -> Result<*const VMFunctionBody, TrampolineError> {
        use std::collections::hash_map::Entry::{Occupied, Vacant};
        Ok(match self.trampoline_park.entry(callee_address) {
            Occupied(entry) => *entry.get(),
            Vacant(entry) => {
                let body = make_trampoline(
                    &*self.isa,
                    &mut self.code_memory,
                    &mut self.fn_builder_ctx,
                    callee_address,
                    signature,
                    value_size,
                )?;
                entry.insert(body);
                body
            }
        })
    }

    /// Create and publish a trampoline for invoking a function.
    pub fn get_published_trampoline(
        &mut self,
        callee_address: *const VMFunctionBody,
        signature: &ir::Signature,
        value_size: usize,
    ) -> Result<*const VMFunctionBody, TrampolineError> {
        let result = self.get_trampoline(callee_address, signature, value_size)?;
        self.publish_compiled_code();
        Ok(result)
    }

    /// Make memory containing compiled code executable.
    pub(crate) fn publish_compiled_code(&mut self) {
        self.code_memory.publish();
    }
}

/// Invoke a function through an `InstanceHandle` identified by an export name.
pub(crate) fn invoke(
    cache: &mut TrampolineCache,
    address: *const VMFunctionBody,
    signature: &ir::Signature,
    callee_vmctx: *mut VMContext,
    args: &[Val],
) -> Result<Box<[Val]>, TrampolineError> {
    // TODO: Support values larger than v128. And pack the values into memory
    // instead of just using fixed-sized slots.
    // Subtract one becase we don't pass the vmctx argument in `values_vec`.
    let value_size = mem::size_of::<VMInvokeArgument>();
    let mut values_vec: Vec<VMInvokeArgument> =
        vec![VMInvokeArgument::new(); max(signature.params.len() - 1, signature.returns.len())];

    // Store the argument values into `values_vec`.
    for (index, arg) in args.iter().enumerate() {
        unsafe {
            let ptr = values_vec.as_mut_ptr().add(index);

            match arg {
                Val::I32(x) => ptr::write(ptr as *mut i32, *x),
                Val::I64(x) => ptr::write(ptr as *mut i64, *x),
                Val::F32(x) => ptr::write(ptr as *mut u32, *x),
                Val::F64(x) => ptr::write(ptr as *mut u64, *x),
            }
        }
    }

    // Get the trampoline to call for this function.
    let exec_code_buf = cache.get_published_trampoline(address, &signature, value_size)?;

    // Call the trampoline.
    if let Err(_message) = unsafe {
        wasmtime_call_trampoline(
            callee_vmctx,
            exec_code_buf,
            values_vec.as_mut_ptr() as *mut u8,
        )
    } {
        return Err(TrampolineError("trap or error during invoke()"));
    }

    // Load the return values out of `values_vec`.
    let values = signature
        .returns
        .iter()
        .enumerate()
        .map(|(index, abi_param)| unsafe {
            let ptr = values_vec.as_ptr().add(index);

            match abi_param.value_type {
                ir::types::I32 => Val::I32(ptr::read(ptr as *const i32)),
                ir::types::I64 => Val::I64(ptr::read(ptr as *const i64)),
                ir::types::F32 => Val::F32(ptr::read(ptr as *const u32)),
                ir::types::F64 => Val::F64(ptr::read(ptr as *const u64)),
                //ir::types::I8X16 => Val::V128(ptr::read(ptr as *const [u8; 16])),
                other => panic!("unsupported value type {:?}", other),
            }
        })
        .collect::<Vec<_>>();

    Ok(values.into_boxed_slice())
}

/// Create a trampoline for invoking a function.
fn make_trampoline(
    isa: &dyn TargetIsa,
    code_memory: &mut CodeMemory,
    fn_builder_ctx: &mut FunctionBuilderContext,
    callee_address: *const VMFunctionBody,
    signature: &ir::Signature,
    value_size: usize,
) -> Result<*const VMFunctionBody, TrampolineError> {
    let pointer_type = isa.pointer_type();
    let mut wrapper_sig = ir::Signature::new(isa.frontend_config().default_call_conv);

    // Add the `vmctx` parameter.
    wrapper_sig.params.push(ir::AbiParam::special(
        pointer_type,
        ir::ArgumentPurpose::VMContext,
    ));
    // Add the `values_vec` parameter.
    wrapper_sig.params.push(ir::AbiParam::new(pointer_type));

    let mut context = Context::new();
    context.func = ir::Function::with_name_signature(ir::ExternalName::user(0, 0), wrapper_sig);

    {
        let mut builder = FunctionBuilder::new(&mut context.func, fn_builder_ctx);
        let block0 = builder.create_ebb();

        builder.append_ebb_params_for_function_params(block0);
        builder.switch_to_block(block0);
        builder.seal_block(block0);

        let (vmctx_ptr_val, values_vec_ptr_val) = {
            let params = builder.func.dfg.ebb_params(block0);
            (params[0], params[1])
        };

        // Load the argument values out of `values_vec`.
        let mflags = ir::MemFlags::trusted();
        let callee_args = signature
            .params
            .iter()
            .enumerate()
            .map(|(i, r)| {
                match r.purpose {
                    // i - 1 because vmctx isn't passed through `values_vec`.
                    ir::ArgumentPurpose::Normal => builder.ins().load(
                        r.value_type,
                        mflags,
                        values_vec_ptr_val,
                        ((i - 1) * value_size) as i32,
                    ),
                    ir::ArgumentPurpose::VMContext => vmctx_ptr_val,
                    other => panic!("unsupported argument purpose {}", other),
                }
            })
            .collect::<Vec<_>>();

        let new_sig = builder.import_signature(signature.clone());

        // TODO: It's possible to make this a direct call. We just need Cranelift
        // to support functions declared with an immediate integer address.
        // ExternalName::Absolute(u64). Let's do it.
        let callee_value = builder.ins().iconst(pointer_type, callee_address as i64);
        let call = builder
            .ins()
            .call_indirect(new_sig, callee_value, &callee_args);

        let results = builder.func.dfg.inst_results(call).to_vec();

        // Store the return values into `values_vec`.
        let mflags = ir::MemFlags::trusted();
        for (i, r) in results.iter().enumerate() {
            builder
                .ins()
                .store(mflags, *r, values_vec_ptr_val, (i * value_size) as i32);
        }

        builder.ins().return_(&[]);
        builder.finalize()
    }

    let mut code_buf = Vec::new();
    let mut unwind_info = Vec::new();
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
        .map_err(|_error| TrampolineError("compile_and_emit"))?;

    context.emit_unwind_info(isa, &mut unwind_info);

    Ok(code_memory
        .allocate_for_function(&CompiledFunction {
            body: code_buf,
            jt_offsets: context.func.jt_offsets,
            unwind_info,
        })
        .map_err(|_message| TrampolineError("allocate_for_function"))?
        .as_ptr())
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
