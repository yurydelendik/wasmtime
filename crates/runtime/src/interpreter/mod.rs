use crate::instance::InstanceHandle;
use crate::vmcontext::VMContext;
use cranelift_codegen::ir;
use cranelift_wasm::FuncIndex;
use eval::Ctx;
use std::ptr;

use wasmeval::*;

mod code_memory;
mod eval;
mod function_table;
mod trampoline;

pub(crate) unsafe fn find_body<'a>(ff: *const u8) -> &'a [u8] {
    // HACK Searching for function blob: header b"asmF"
    let mut start = None;
    for i in 0..1000 {
        if *ff.offset(i) != b'a'
            || *ff.offset(i + 3) != b'F'
            || *ff.offset(i + 1) != b's'
            || *ff.offset(i + 2) != b'm'
        {
            continue;
        }
        let len = *(ff.offset(i + 4) as *const u32);
        let ptr = ff.offset(i + 8) as *const u8;
        start = Some(std::slice::from_raw_parts(ptr, len as usize));
        break;
    }
    if start.is_none() {
        panic!("asmF not found");
    }
    start.unwrap()
}

pub(crate) unsafe fn read_val(ptr: *const u8, ty: &ir::Type) -> Val {
    match *ty {
        ir::types::I32 => Val::I32(ptr::read(ptr as *const i32)),
        ir::types::I64 => Val::I64(ptr::read(ptr as *const i64)),
        ir::types::F32 => Val::F32(ptr::read(ptr as *const u32)),
        ir::types::F64 => Val::F64(ptr::read(ptr as *const u64)),
        other => panic!("unsupported value type {:?}", other),
    }
}

pub(crate) unsafe fn write_val(ptr: *mut u8, val: &Val) {
    match val {
        Val::I32(x) => ptr::write(ptr as *mut i32, *x),
        Val::I64(x) => ptr::write(ptr as *mut i64, *x),
        Val::F32(x) => ptr::write(ptr as *mut u32, *x),
        Val::F64(x) => ptr::write(ptr as *mut u64, *x),
    }
}

/// Internal eval intrinsic method.
/// TODO docs
#[no_mangle]
pub unsafe extern "C" fn wasmtime_eval(vmctx: *mut VMContext, args: *mut i64, call_id: u32) -> u32 {
    let _instance = (&mut *vmctx).instance();
    let mut handle = InstanceHandle::from_vmctx(vmctx);
    let module = handle.module_ref();
    let func_index = FuncIndex::from_u32(call_id);
    let sig = &module.signatures[module.functions[func_index]];
    let ff_index = module.defined_func_index(func_index).unwrap();
    let ff = handle.finished_function(ff_index) as *const u8;

    let body = find_body(ff);

    let mut params = Vec::with_capacity(sig.params.len() - 1);
    for index in 1..sig.params.len() {
        let ptr = args.add(index - 1);
        params.push(read_val(ptr as *const u8, &sig.params[index].value_type));
    }

    let mut returns = vec![Val::I32(0); sig.returns.len()];

    {
        let handle_clone = handle.clone();
        let host_state = handle.host_state_boxed();
        if host_state.is::<()>() {
            *host_state = Box::new(Ctx::new(handle_clone));
        }
    }

    let ctx = handle
        .host_state()
        .downcast_ref::<Ctx>()
        .expect("Interpreter context");
    match eval(ctx, &params, &mut returns, &body) {
        Ok(()) => {
            for (index, ret) in returns.iter().enumerate() {
                let ptr = args.add(index);
                write_val(ptr as *mut u8, ret);
            }
            0
        }
        Err(_err) => 1,
    }
}
