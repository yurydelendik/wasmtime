use super::trampoline::{TrampolineCache, TrampolineError};
use crate::{wasmtime_call_trampoline, VMContext, VMFunctionBody, VMInvokeArgument};
use cranelift_codegen::{ir, isa};
use std::cmp::max;
use std::collections::HashMap;
use std::ptr;
use target_lexicon::HOST;
use wasmeval::Val;

macro_rules! wrapper_function {
    (fn (ctx, $($a:ident),*) -> $r:ident) => {{
        unsafe extern "C" fn f(
            vmctx: *mut VMContext,
            values_vec: *mut VMInvokeArgument,
        ) -> i32 {
            type F = extern "C" fn(vmctx: *mut VMContext, $($a),*) -> $r;
            let mut p = values_vec;
            let callee: F = std::mem::transmute(*(values_vec as *const *const VMFunctionBody));
            let res = callee(
                vmctx, $(
                    {
                        p = p.add(1);
                        *(p as *const $a)
                    }
                ),*
            );
            *(values_vec.add(0) as *mut $r) = res;
            1 // success
        }
        f as *const u8 as *const VMFunctionBody
    }};
}

macro_rules! to_ir_type {
    (i32) => {
        ir::types::I32
    };
    (i64) => {
        ir::types::I64
    };
}

macro_rules! target_signature {
    (fn (ctx, $($a:ident),*) -> $r:ident; $conv:expr) => {
        ir::Signature {
            params: vec![
                ir::AbiParam::special(ir::types::I64, ir::ArgumentPurpose::VMContext),
                $(
                    ir::AbiParam::new(to_ir_type!($a))
                ),*
            ],
            returns: vec![ir::AbiParam::new(to_ir_type!($r))],
            call_conv: $conv,
        }
    }
}

fn get_trampolines() -> &'static HashMap<ir::Signature, *const VMFunctionBody> {
    static mut TRAMPOLINES: Option<HashMap<ir::Signature, *const VMFunctionBody>> = None;
    unsafe {
        TRAMPOLINES.get_or_insert_with(|| {
            let mut dict = HashMap::new();
            let call_conv = isa::CallConv::triple_default(&HOST);

            macro_rules! add_to_dict {
                (($($a:ident),*) -> $r:ident) => {
                    dict.insert(
                        target_signature!(fn ($($a),*) -> $r; call_conv),
                        wrapper_function!(fn ($($a),*) -> $r)
                    );
                }
            }

            // pond
            add_to_dict!((ctx, i32, i32) -> i32);
            add_to_dict!((ctx, i32, i32, i32) -> i32);
            add_to_dict!((ctx, i32, i32, i32, i32) -> i32);

            dict
        })
    }
}

pub(crate) fn get_trampoline(signature: &ir::Signature) -> *const VMFunctionBody {
    if let Some(b) = get_trampolines().get(&signature) {
        return *b;
    }
    unimplemented!("{:?}", signature);
}

pub(crate) fn invoke(
    _cache: &mut TrampolineCache,
    address: *const VMFunctionBody,
    signature: &ir::Signature,
    callee_vmctx: *mut VMContext,
    args: &[Val],
) -> Result<Box<[Val]>, TrampolineError> {
    // TODO: Support values larger than v128. And pack the values into memory
    // instead of just using fixed-sized slots.
    // Subtract one becase we don't pass the vmctx argument in `values_vec`.
    let mut values_vec: Vec<VMInvokeArgument> =
        vec![VMInvokeArgument::new(); max(signature.params.len(), signature.returns.len())];

    unsafe {
        ptr::write(values_vec.as_mut_ptr() as *mut _, address);
    }

    // Store the argument values into `values_vec`.
    for (index, arg) in args.iter().enumerate() {
        unsafe {
            let ptr = values_vec.as_mut_ptr().add(index + 1);

            match arg {
                Val::I32(x) => ptr::write(ptr as *mut i32, *x),
                Val::I64(x) => ptr::write(ptr as *mut i64, *x),
                Val::F32(x) => ptr::write(ptr as *mut u32, *x),
                Val::F64(x) => ptr::write(ptr as *mut u64, *x),
            }
        }
    }

    // Get the trampoline to call for this function.
    //let exec_code_buf = cache.get_published_trampoline(address, &signature, value_size)?;
    let exec_code_buf = get_trampoline(&signature);

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
