use anyhow::Result;
use std::ptr::NonNull;
use wasmtime_aot_runtime::{instantiate, read_compiled_module, WasiCtxData};
use wasmtime_environ::entity::EntityRef;
use wasmtime_environ::wasm::{EntityIndex, MemoryIndex, TableIndex};
use wasmtime_runtime::{
    Export, InstanceHandle, TableElement, VMCallerCheckedAnyfunc, VMContext, VMFunctionBody,
    VMSharedSignatureIndex,
};

extern "C" fn callb(_vmctx: *mut VMContext, caller_vmctx: *mut VMContext, i: i32) -> i32 {
    let caller = unsafe { InstanceHandle::from_vmctx(caller_vmctx) };
    let m = match caller.lookup_by_declaration(&EntityIndex::Memory(MemoryIndex::new(0))) {
        Export::Memory(mi) => mi.definition,
        _ => panic!(),
    };
    let p = unsafe { (*m).base };
    eprintln!("+++{}+{:?}+++", i, p);
    i + 42
}

fn get_linked_wasm_meta() -> *const u8 {
    #[link(name = "foo", kind = "static")]
    extern "C" {
        static wasmtime_meta: u8;
    }
    unsafe { &wasmtime_meta }
}

fn main() -> Result<()> {
    let wasi_ctx = WasiCtxData::new()?;

    let compiled_module = read_compiled_module(get_linked_wasm_meta())?;

    let instance = instantiate(&compiled_module, &wasi_ctx)?;
    let instance_ctx = instance.vmctx_ptr();

    let run_index = instance.exports().find(|e| e.0 == "bar").unwrap().1;
    let run_fn_ptr: *const VMFunctionBody = unsafe {
        match instance.lookup_by_declaration(run_index) {
            Export::Function(ef) => {
                let r = ef.anyfunc.as_ref();
                assert!(instance_ctx == r.vmctx);
                r.func_ptr.as_ptr()
            }
            _ => panic!(),
        }
    };

    let mut callb_fn = VMCallerCheckedAnyfunc {
        func_ptr: NonNull::new(callb as *const VMFunctionBody as *mut _).unwrap(),
        type_index: VMSharedSignatureIndex::new(0),
        vmctx: std::ptr::null_mut(),
    };
    let callb_idx = instance
        .table_grow(TableIndex::new(0), 1, TableElement::FuncRef(&mut callb_fn))
        .expect("table grown");

    type RunFn = extern "C" fn(*mut VMContext, *mut VMContext, i32, i32) -> i32;
    let run: RunFn = unsafe { std::mem::transmute(run_fn_ptr) };

    let res = run(instance_ctx, std::ptr::null_mut(), 2, callb_idx as i32);
    println!("{}", res);

    if let Export::Memory(m) =
        instance.lookup_by_declaration(instance.exports().find(|e| e.0 == "memory").unwrap().1)
    {
        let p = unsafe { (*m.definition).base };
        eprintln!("mem: {:?}", p);
    }

    Ok(())
}
