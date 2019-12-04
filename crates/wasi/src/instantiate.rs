use super::syscalls;
use cranelift_codegen::ir::types;
use cranelift_codegen::{ir, isa};
use cranelift_entity::PrimaryMap;
use cranelift_wasm::DefinedFuncIndex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::rc::Rc;
use target_lexicon::HOST;
use wasi_common::{WasiCtx, WasiCtxBuilder};
use wasmtime_environ::{translate_signature, Export, Module};
use wasmtime_runtime::{Imports, InstanceHandle, InstantiationError, VMFunctionBody};

/// Creates `wasmtime::Instance` object implementing the "wasi" interface.
pub fn create_wasi_instance(
    store: &wasmtime::HostRef<wasmtime::Store>,
    preopened_dirs: &[(String, File)],
    argv: &[String],
    environ: &[(String, String)],
) -> Result<wasmtime::Instance, InstantiationError> {
    let global_exports = store.borrow().global_exports().clone();
    let wasi = instantiate_wasi(global_exports, preopened_dirs, argv, environ)?;
    let instance = wasmtime::Instance::from_handle(&store, wasi);
    Ok(instance)
}

/// Return an instance implementing the "wasi" interface.
pub fn instantiate_wasi(
    global_exports: Rc<RefCell<HashMap<String, Option<wasmtime_runtime::Export>>>>,
    preopened_dirs: &[(String, File)],
    argv: &[String],
    environ: &[(String, String)],
) -> Result<InstanceHandle, InstantiationError> {
    let mut wasi_ctx_builder = WasiCtxBuilder::new()
        .inherit_stdio()
        .args(argv)
        .envs(environ);

    for (dir, f) in preopened_dirs {
        wasi_ctx_builder = wasi_ctx_builder.preopened_dir(
            f.try_clone().map_err(|err| {
                InstantiationError::Resource(format!(
                    "couldn't clone an instance handle to pre-opened dir: {}",
                    err
                ))
            })?,
            dir,
        );
    }

    let wasi_ctx = wasi_ctx_builder.build().map_err(|err| {
        InstantiationError::Resource(format!("couldn't assemble WASI context object: {}", err))
    })?;
    instantiate_wasi_with_context(global_exports, wasi_ctx)
}

macro_rules! for_each_func {
    ($m:ident!) => {
        $m!(args_get);
        $m!(args_sizes_get);
        $m!(clock_res_get);
        $m!(clock_time_get);
        $m!(environ_get);
        $m!(environ_sizes_get);
        $m!(fd_prestat_get);
        $m!(fd_prestat_dir_name);
        $m!(fd_close);
        $m!(fd_datasync);
        $m!(fd_pread);
        $m!(fd_pwrite);
        $m!(fd_read);
        $m!(fd_renumber);
        $m!(fd_seek);
        $m!(fd_tell);
        $m!(fd_fdstat_get);
        $m!(fd_fdstat_set_flags);
        $m!(fd_fdstat_set_rights);
        $m!(fd_sync);
        $m!(fd_write);
        $m!(fd_advise);
        $m!(fd_allocate);
        $m!(path_create_directory);
        $m!(path_link);
        $m!(path_open);
        $m!(fd_readdir);
        $m!(path_readlink);
        $m!(path_rename);
        $m!(fd_filestat_get);
        $m!(fd_filestat_set_times);
        $m!(fd_filestat_set_size);
        $m!(path_filestat_get);
        $m!(path_filestat_set_times);
        $m!(path_symlink);
        $m!(path_unlink_file);
        $m!(path_remove_directory);
        $m!(poll_oneoff);
        $m!(proc_exit);
        $m!(proc_raise);
        $m!(random_get);
        $m!(sched_yield);
        $m!(sock_recv);
        $m!(sock_send);
        $m!(sock_shutdown);
    };
}

pub(crate) fn get_exports() -> Vec<(String, ir::Signature)> {
    let pointer_type = types::Type::triple_pointer_type(&HOST);
    let call_conv = isa::CallConv::triple_default(&HOST);
    let mut exports = Vec::new();

    macro_rules! export {
        ($name:ident) => {{
            let sig = translate_signature(
                ir::Signature {
                    params: syscalls::$name::params()
                        .into_iter()
                        .map(ir::AbiParam::new)
                        .collect(),
                    returns: syscalls::$name::results()
                        .into_iter()
                        .map(ir::AbiParam::new)
                        .collect(),
                    call_conv,
                },
                pointer_type,
            );
            exports.push((stringify!($name).to_owned(), sig));
        }};
    }

    for_each_func!(export!);

    exports
}

/// Return an instance implementing the "wasi" interface.
///
/// The wasi context is configured by
pub fn instantiate_wasi_with_context(
    global_exports: Rc<RefCell<HashMap<String, Option<wasmtime_runtime::Export>>>>,
    wasi_ctx: WasiCtx,
) -> Result<InstanceHandle, InstantiationError> {
    let pointer_type = types::Type::triple_pointer_type(&HOST);
    let mut module = Module::new();
    let mut finished_functions: PrimaryMap<DefinedFuncIndex, *const VMFunctionBody> =
        PrimaryMap::new();
    let call_conv = isa::CallConv::triple_default(&HOST);

    macro_rules! signature {
        ($name:ident) => {{
            let sig = module.signatures.push(translate_signature(
                ir::Signature {
                    params: syscalls::$name::params()
                        .into_iter()
                        .map(ir::AbiParam::new)
                        .collect(),
                    returns: syscalls::$name::results()
                        .into_iter()
                        .map(ir::AbiParam::new)
                        .collect(),
                    call_conv,
                },
                pointer_type,
            ));
            let func = module.functions.push(sig);
            module
                .exports
                .insert(stringify!($name).to_owned(), Export::Function(func));
            finished_functions.push(syscalls::$name::SHIM as *const VMFunctionBody);
        }};
    }

    for_each_func!(signature!);

    let imports = Imports::none();
    let data_initializers = Vec::new();
    let signatures = PrimaryMap::new();

    InstanceHandle::new(
        Rc::new(module),
        global_exports,
        finished_functions.into_boxed_slice(),
        imports,
        &data_initializers,
        signatures.into_boxed_slice(),
        None,
        Box::new(wasi_ctx),
    )
}
