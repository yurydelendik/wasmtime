use anyhow::Result;
use wasi_cap_std_sync::WasiCtxBuilder;

use wasmtime_environ::wasm::{EntityType, SignatureIndex};
use wasmtime_environ::{entity::EntityRef, Initializer, Module};
use wasmtime_jit::CompiledModule;
use wasmtime_runtime::{
    Imports, InstanceAllocationRequest, InstanceAllocator, InstanceHandle,
    OnDemandInstanceAllocator, StackMapRegistry, VMContext, VMExternRefActivationsTable,
    VMFunctionBody, VMFunctionImport, VMInterrupts, VMSharedSignatureIndex,
};

use bincode::Options;
use std::cell::RefCell;
use std::ptr::{self, NonNull};
use std::rc::Rc;
use wasmtime_wasi::snapshots::preview_1;

pub fn read_compiled_module(meta_ptr: *const u8) -> Result<std::sync::Arc<CompiledModule>> {
    let (meta_data, func_ptrs) = unsafe {
        let meta_len = ptr::read(meta_ptr as *const u64);
        (
            std::slice::from_raw_parts(meta_ptr.add(8), meta_len as usize),
            meta_ptr.add(8 + meta_len as usize) as *const u64,
        )
    };

    let module = bincode::DefaultOptions::new()
        .with_varint_encoding()
        .deserialize::<Module>(meta_data)?;

    let (funcs, trampolines) = unsafe {
        let def_functions_len = module.functions.len() - module.num_imported_funcs;
        let trampolines_ptr = func_ptrs.add(def_functions_len);
        // FIXME proper value for trampolines_len
        let trampolines_len = module.functions.values().max().unwrap().index() + 1;
        let mut funcs = Vec::with_capacity(def_functions_len);
        for i in 0..def_functions_len {
            funcs.push(ptr::read(func_ptrs.add(i)) as usize as *const u8);
        }
        let mut trampolines = Vec::with_capacity(trampolines_len);
        for i in 0..trampolines_len {
            trampolines.push(trampolines_ptr.add(i) as usize as *const u8);
        }
        (funcs, trampolines)
    };

    let compiled_module = CompiledModule::from_raw_parts(module, funcs, trampolines)?;
    Ok(compiled_module)
}

pub struct WasiCtxData(Box<dyn std::any::Any + 'static>);

impl WasiCtxData {
    pub fn new() -> Result<Self> {
        let wasi_ctx0: Box<dyn std::any::Any + 'static> = Box::new(Rc::new(RefCell::new(
            WasiCtxBuilder::new()
                .inherit_stdio()
                .inherit_args()?
                .build()?,
        )));
        Ok(Self(wasi_ctx0))
    }
    pub fn ptr(&self) -> *const u8 {
        &self.0 as *const Box<_> as *const u8
    }
}

pub fn instantiate(
    compiled_module: &CompiledModule,
    wasi_ctx: &WasiCtxData,
) -> Result<InstanceHandle> {
    // HACK masking the above Box as VMContext
    // Raw generated function (see comment below) will know how to handle it
    let wasi_ctx0_ptr = wasi_ctx.ptr();

    let mut import_functions = Vec::new();
    for imp in compiled_module.module().initializers.iter() {
        if let Initializer::Import {
            name: _,
            field,
            index,
        } = imp
        {
            let ty = match compiled_module.module().type_of(*index) {
                EntityType::Function(sig) => sig,
                _ => panic!(),
            };
            // The _raw_wasi_snapshot_preview1_XXXX functions are static via hack
            let body = match field.as_ref().unwrap().as_str() {
                "fd_close" => preview_1::_raw_wasi_snapshot_preview1_fd_close as *const u8,
                "fd_write" => preview_1::_raw_wasi_snapshot_preview1_fd_write as *const u8,
                "fd_seek" => preview_1::_raw_wasi_snapshot_preview1_fd_seek as *const u8,
                _ => {
                    panic!("{:?} {:?}", field, ty);
                }
            };
            import_functions.push(VMFunctionImport {
                body: NonNull::new(body as *const VMFunctionBody as *mut _).unwrap(),
                vmctx: wasi_ctx0_ptr as *const VMContext as *mut _,
            });
        }
    }
    let lookup_shared_signature = |sig: SignatureIndex| -> VMSharedSignatureIndex {
        VMSharedSignatureIndex::new(sig.index() as u32)
    };
    let mut externref_activations_table = VMExternRefActivationsTable::new();
    let mut stack_map_registry = StackMapRegistry::default();

    let imports = Imports {
        functions: &import_functions,
        tables: &[],
        memories: &[],
        globals: &[],
    };
    let ints = VMInterrupts::default();

    unsafe {
        let allocator = OnDemandInstanceAllocator::new(None, /* stack_size = */ 0);

        let instance = allocator.allocate(InstanceAllocationRequest {
            module: compiled_module.module().clone(),
            finished_functions: compiled_module.finished_functions(),
            imports,
            lookup_shared_signature: &lookup_shared_signature,
            host_state: Box::new(()),
            interrupts: &ints,
            externref_activations_table: &mut externref_activations_table,
            stack_map_registry: &mut stack_map_registry,
        })?;

        allocator.initialize(&instance, false)?;

        Ok(instance)
    }
    // TODO start function or __wasm_call_ctors ?
}