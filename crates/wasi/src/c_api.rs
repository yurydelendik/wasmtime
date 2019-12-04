use crate::instantiate::{get_exports, instantiate_wasi};
use std::ptr;
use wasmtime::wasm::*;
use wasmtime::{ExportType, ExternType, FuncType, HostRef, Instance, Module, Name};

unsafe extern "C" fn instantiate_wasi_callback(
    store: *mut wasm_store_t,
    _module: *const wasm_module_t,
    _imports: *const *const wasm_extern_t,
    result: *mut *mut wasm_trap_t,
) -> *mut wasm_instance_t {
    let store = &(*store).store;
    let global_exports = store.borrow().global_exports().clone();
    // TODO pull configuration from _module or _imports
    let preopen_dirs = vec![];
    let argv = vec![];
    let environ = vec![];
    let handle =
        instantiate_wasi(global_exports, &preopen_dirs, &argv, &environ).expect("wasi instance");
    if !result.is_null() {
        (*result) = ptr::null_mut();
    }
    let instance = Box::new(wasm_instance_t {
        instance: HostRef::new(Instance::from_handle(store, handle)),
    });
    Box::into_raw(instance)
}

#[no_mangle]
pub unsafe extern "C" fn wasmtime_wasi_module_new(store: *mut wasm_store_t) -> *mut wasm_module_t {
    let store = &(*store).store;
    let imports = Vec::new();
    let mut exports = Vec::new();
    for (name, signature) in get_exports() {
        let _ = store.borrow_mut().register_wasmtime_signature(&signature);
        let ft = FuncType::from_wasmtime_signature(signature);
        let ext = ExternType::ExternFunc(ft);
        exports.push(ExportType::new(Name::new(&name), ext));
    }
    let module = Module::from_exports(store, exports.clone().into_boxed_slice());
    let exports = exports
        .into_iter()
        .map(|e| wasm_exporttype_t {
            ty: e,
            name_cache: None,
            type_cache: None,
        })
        .collect::<Vec<_>>();
    let module = Box::new(wasm_module_t {
        module: HostRef::new(module),
        imports,
        exports,
        instantiate_callback: Some(instantiate_wasi_callback),
    });
    Box::into_raw(module)
}
