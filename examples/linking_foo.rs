use anyhow::Result;
use std::ptr::NonNull;
use wasmtime_aot_runtime::{instantiate, read_compiled_module, WasiCtxData};
use wasmtime_environ::entity::EntityRef;
use wasmtime_environ::wasm::{EntityIndex, MemoryIndex, TableIndex, WasmFuncType, WasmType};
use wasmtime_environ::TypeTables;
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
        static aot_wasmtime_meta: u8;
    }
    unsafe { &aot_wasmtime_meta }
}

#[link(name = "foo", kind = "static")]
#[allow(improper_ctypes)]
extern "C" {
    fn aot_bar(vmctx: *mut VMContext, caller_vmctx: *mut VMContext, a: i32, c: i32) -> i32;
}

fn lookup_type_index(types: &TypeTables, ty: WasmFuncType) -> VMSharedSignatureIndex {
    for (i, c) in types.wasm_signatures.iter() {
        if c == &ty {
            return VMSharedSignatureIndex::new(i.index() as u32);
        }
    }
    panic!("type index not found for {:?}", ty);
}

fn main() -> Result<()> {
    let wasi_ctx = WasiCtxData::new()?;

    let (compiled_module, types) = read_compiled_module(get_linked_wasm_meta())?;

    let instance = instantiate(&compiled_module, &wasi_ctx)?;
    let instance_ctx = instance.vmctx_ptr();

    let mut callb_fn = VMCallerCheckedAnyfunc {
        func_ptr: NonNull::new(callb as *const VMFunctionBody as *mut _).unwrap(),
        type_index: lookup_type_index(
            &types,
            WasmFuncType {
                params: Box::new([WasmType::I32]),
                returns: Box::new([WasmType::I32]),
            },
        ),
        vmctx: std::ptr::null_mut(),
    };
    let callb_idx = instance
        .table_grow(TableIndex::new(0), 1, TableElement::FuncRef(&mut callb_fn))
        .expect("table grown");

    let res = unsafe { aot_bar(instance_ctx, std::ptr::null_mut(), 2, callb_idx as i32) };
    println!("{}", res);

    if let Export::Memory(m) =
        instance.lookup_by_declaration(instance.exports().find(|e| e.0 == "memory").unwrap().1)
    {
        let p = unsafe { (*m.definition).base };
        eprintln!("mem: {:?}", p);
    }

    Ok(())
}
