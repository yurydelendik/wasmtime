//! Implementation of eval function.

use crate::instance::InstanceHandle;
use crate::vmcontext::{
    VMContext, VMFunctionBody, VMFunctionImport, VMGlobalDefinition, VMMemoryDefinition,
};
use std::cell::{Ref, RefCell};
use std::convert::TryFrom;
use std::rc::Rc;
use wasmtime_environ::ir;
use wasmtime_environ::wasm::{DefinedTableIndex, FuncIndex, SignatureIndex};

use wasmeval::*;

//use super::trampoline::{invoke, TrampolineCache};
use super::trampoline::TrampolineCache;
use super::trampoline_sea::invoke;

use super::{find_body, read_val, write_val};

struct InterpreterMemory {
    memory: *const VMMemoryDefinition,
    vmctx: *mut VMContext,
}

#[inline]
fn combine_offsets(memarg: &MemoryImmediate, offset: u32) -> usize {
    memarg.offset as usize + offset as usize
}

impl Memory for InterpreterMemory {
    fn current(&self) -> u32 {
        let current = unsafe { (*self.memory).current_length };
        (current >> 16) as u32
    }
    fn grow(&mut self, delta: u32) -> u32 {
        let mut handle = unsafe { InstanceHandle::from_vmctx(self.vmctx) };
        let memory_index = unsafe { handle.memory_index(&*self.memory) };
        match handle.memory_grow(memory_index, delta) {
            Some(old) => old,
            None => std::u32::MAX,
        }
    }
    fn content_ptr(&self, memarg: &MemoryImmediate, offset: u32, size: u32) -> *const u8 {
        let offset = combine_offsets(memarg, offset);
        if offset + size as usize > unsafe { (*self.memory).current_length } {
            return std::ptr::null();
        }
        let base = unsafe { (*self.memory).base };
        unsafe { base.offset(offset as isize) }
    }
    fn content_ptr_mut(&mut self, memarg: &MemoryImmediate, offset: u32, size: u32) -> *mut u8 {
        let offset = combine_offsets(memarg, offset);
        if offset + size as usize > unsafe { (*self.memory).current_length } {
            return std::ptr::null_mut();
        }
        let base = unsafe { (*self.memory).base };
        unsafe { base.offset(offset as isize) }
    }
    fn clone_from_slice(&mut self, offset: u32, chunk: &[u8]) {
        let offset = offset as usize;
        let base = unsafe { (*self.memory).base };
        unsafe {
            let ptr = base.offset(offset as isize);
            // check self.memory.current_length
            std::slice::from_raw_parts_mut(ptr, chunk.len()).clone_from_slice(chunk);
        }
    }
}

struct InterpreterGlobal {
    global: *const VMGlobalDefinition,
    ty: ir::Type,
}

impl Global for InterpreterGlobal {
    fn content(&self) -> Val {
        // HACK use VMGlobalDefinition pointer to access storage field.
        unsafe { read_val(self.global as *const u8, &self.ty) }
    }
    fn set_content(&mut self, val: &Val) {
        // HACK use VMGlobalDefinition pointer to access storage field.
        unsafe {
            write_val(self.global as *mut u8, val);
        }
    }
}

struct InterpreterTable {
    vmctx: *mut VMContext,
    table_index: DefinedTableIndex,
}

impl Table for InterpreterTable {
    fn get_func(&self, _index: u32) -> Result<Option<Rc<RefCell<dyn Func>>>, TableOutOfBounds> {
        unimplemented!();
    }
    fn get_func_with_type(
        &self,
        index: u32,
        type_index: u32,
    ) -> Result<Option<Rc<RefCell<dyn Func>>>, TableOutOfBounds> {
        let handle = unsafe { InstanceHandle::from_vmctx(self.vmctx) };
        let module = handle.module_ref();
        let sig = module.signatures[SignatureIndex::from_u32(type_index)].clone();

        if let Some(item) = handle.table_get(self.table_index, index) {
            Ok(Some(Rc::new(RefCell::new(TableFunc {
                vmctx: self.vmctx,
                address: item.func_ptr,
                sig,
                callee_vmctx: item.vmctx,
            }))))
        } else {
            Err(TableOutOfBounds)
        }
    }
    fn set_func(
        &mut self,
        _index: u32,
        _f: Option<Rc<RefCell<dyn Func>>>,
    ) -> Result<(), TableOutOfBounds> {
        unimplemented!();
    }
}

struct TableFunc {
    vmctx: *mut VMContext,
    address: *const VMFunctionBody,
    sig: ir::Signature,
    callee_vmctx: *mut VMContext,
}

impl Func for TableFunc {
    fn params_arity(&self) -> usize {
        self.sig.params.len() - 1
    }
    fn results_arity(&self) -> usize {
        self.sig.returns.len()
    }
    fn call(&self, params: &[Val], results: &mut [Val]) -> Result<(), Trap> {
        let mut handle = unsafe { InstanceHandle::from_vmctx(self.vmctx) };
        let ctx = handle
            .host_state()
            .downcast_mut::<Ctx>()
            .expect("Interpreter context");
        match invoke(
            &mut ctx.trampolines,
            self.address,
            &self.sig,
            self.callee_vmctx,
            params,
        ) {
            Ok(r) => {
                results.clone_from_slice(&r);
                Ok(())
            }
            Err(_) => {
                unimplemented!();
            }
        }
    }
}

fn get_func_info(
    handle: &mut InstanceHandle,
    func_index: FuncIndex,
) -> (*const VMFunctionBody, ir::Signature, *mut VMContext) {
    let module = handle.module_ref();
    let vmctx = handle.vmctx_ptr();
    match module.defined_func_index(func_index) {
        Some(_) => unimplemented!(),
        None => {
            let sig = module.signatures[module.functions[func_index]].clone();
            let import = unsafe {
                let offsets = &(*handle.instance).offsets;
                let ptr = (vmctx as *const VMContext as *const u8)
                    .add(usize::try_from(offsets.vmctx_vmfunction_import(func_index)).unwrap());
                &*(ptr as *const VMFunctionImport)
            };
            (import.body, sig, import.vmctx)
        }
    }
}

struct InterpreterFunc {
    vmctx: *mut VMContext,
    func_index: FuncIndex,
    sig: ir::Signature,
}

impl Func for InterpreterFunc {
    fn params_arity(&self) -> usize {
        self.sig.params.len() - 1
    }
    fn results_arity(&self) -> usize {
        self.sig.returns.len()
    }
    fn call(&self, params: &[Val], results: &mut [Val]) -> Result<(), Trap> {
        let mut handle = unsafe { InstanceHandle::from_vmctx(self.vmctx) };
        let module = handle.module_ref();
        match module.defined_func_index(self.func_index) {
            Some(ff_index) => {
                let ff = handle.finished_function(ff_index) as *const u8;
                let body = unsafe { find_body(ff) };
                let ctx = handle
                    .host_state()
                    .downcast_ref::<Ctx>()
                    .expect("Interpreter context");
                match eval(ctx, params, results, &body) {
                    Err(TrapOrParserError::Trap(t)) => Err(t),
                    Err(TrapOrParserError::ParserError(_)) => {
                        unimplemented!();
                    }
                    Ok(()) => Ok(()),
                }
            }
            None => {
                let mut handle_clone = handle.clone();
                let ctx = handle_clone
                    .host_state()
                    .downcast_mut::<Ctx>()
                    .expect("Interpreter context");
                let (address, signature, callee_vmctx) =
                    get_func_info(&mut handle, self.func_index);
                match invoke(
                    &mut ctx.trampolines,
                    address,
                    &signature,
                    callee_vmctx,
                    params,
                ) {
                    Ok(r) => {
                        results.clone_from_slice(&r);
                        Ok(())
                    }
                    Err(_) => {
                        unimplemented!();
                    }
                }
            }
        }
    }
}

struct InterpreterFuncType {
    ty: RefCell<data::FuncType>,
}

fn into_data_type(ty: &ir::AbiParam) -> data::Type {
    debug_assert!(ty.purpose == ir::ArgumentPurpose::Normal);
    match ty.value_type {
        ir::types::I32 => data::Type::I32,
        ir::types::I64 => data::Type::I64,
        ir::types::F32 => data::Type::F32,
        ir::types::F64 => data::Type::F64,
        _ => unimplemented!(),
    }
}

impl InterpreterFuncType {
    pub fn new(sig: &ir::Signature) -> Self {
        let params = sig.params[1..]
            .iter()
            .map(into_data_type)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let returns = sig
            .returns
            .iter()
            .map(into_data_type)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        InterpreterFuncType {
            ty: RefCell::new(data::FuncType {
                form: data::Type::Func,
                params,
                returns,
            }),
        }
    }
}

impl FuncType for InterpreterFuncType {
    fn ty(&self) -> Ref<data::FuncType> {
        self.ty.borrow()
    }
}

pub(crate) struct Ctx {
    globals: Vec<Rc<RefCell<dyn Global>>>,
    memories: Vec<Rc<RefCell<dyn Memory>>>,
    funcs: Vec<Rc<RefCell<dyn Func>>>,
    tables: Vec<Rc<RefCell<dyn Table>>>,
    func_types: Vec<Rc<RefCell<dyn FuncType>>>,
    trampolines: TrampolineCache,
}

impl Ctx {
    pub fn new(handle: InstanceHandle) -> Self {
        let vmctx = handle.clone().vmctx_mut_ptr();
        let offsets = unsafe { &(*handle.instance).offsets };
        let module = handle.module();

        let mut func_types: Vec<Rc<RefCell<dyn FuncType>>> = Vec::new();
        for (_, s) in module.signatures.iter() {
            func_types.push(Rc::new(RefCell::new(InterpreterFuncType::new(s))));
        }

        let mut memories: Vec<Rc<RefCell<dyn Memory>>> = Vec::new();
        for _i in 0..module.imported_memories.len() {
            unimplemented!("imported_memories");
        }
        for (index, _) in module.memory_plans.iter() {
            unsafe {
                let ptr = (vmctx as *const VMContext as *const u8).add(
                    usize::try_from(
                        offsets
                            .vmctx_vmmemory_definition(module.defined_memory_index(index).unwrap()),
                    )
                    .unwrap(),
                );
                memories.push(Rc::new(RefCell::new(InterpreterMemory {
                    memory: &*(ptr as *const VMMemoryDefinition),
                    vmctx,
                })));
            }
        }

        let mut globals: Vec<Rc<RefCell<dyn Global>>> = Vec::new();
        for _i in 0..module.imported_globals.len() {
            unimplemented!("imported_globals");
        }
        for (index, g) in module.globals.iter() {
            unsafe {
                let ptr = (vmctx as *const VMContext as *const u8).add(
                    usize::try_from(
                        offsets
                            .vmctx_vmglobal_definition(module.defined_global_index(index).unwrap()),
                    )
                    .unwrap(),
                );
                globals.push(Rc::new(RefCell::new(InterpreterGlobal {
                    global: &*(ptr as *const VMGlobalDefinition),
                    ty: g.ty.clone(),
                })));
            }
        }

        let mut tables: Vec<Rc<RefCell<dyn Table>>> = Vec::new();
        for _i in 0..module.imported_tables.len() {
            unimplemented!("imported_tables");
        }
        for (index, _t) in module.table_plans.iter() {
            tables.push(Rc::new(RefCell::new(InterpreterTable {
                vmctx,
                table_index: module.defined_table_index(index).unwrap(),
            })));
        }

        let mut funcs: Vec<Rc<RefCell<dyn Func>>> = Vec::new();
        for (index, sig_id) in module.functions.iter() {
            let sig = module.signatures[*sig_id].clone();
            funcs.push(Rc::new(RefCell::new(InterpreterFunc {
                vmctx,
                func_index: index,
                sig,
            })));
        }

        let trampolines = TrampolineCache::new();

        Self {
            globals,
            memories,
            funcs,
            tables,
            func_types,
            trampolines,
        }
    }
}

impl EvalContext for Ctx {
    fn get_function(&self, index: u32) -> Rc<RefCell<dyn Func>> {
        self.funcs[index as usize].clone()
    }
    fn get_global(&self, index: u32) -> Rc<RefCell<dyn Global>> {
        self.globals[index as usize].clone()
    }
    fn get_memory(&self) -> Rc<RefCell<dyn Memory>> {
        self.memories[0].clone()
    }
    fn get_table(&self, index: u32) -> Rc<RefCell<dyn Table>> {
        self.tables[index as usize].clone()
    }
    fn get_type(&self, index: u32) -> Rc<RefCell<dyn FuncType>> {
        self.func_types[index as usize].clone()
    }
}
