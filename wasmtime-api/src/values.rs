use crate::externals::Func;
use crate::r#ref::Ref;
use crate::types::ValType;
use std::any::Any;
use std::fmt;
use std::ptr;
use std::rc::Rc;

use cranelift_codegen::ir;
use wasmtime_jit::RuntimeValue;

#[derive(Clone)]
pub enum AnyRef {
    Null,
    Rc(Rc<dyn Any>),
    Func(Ref<Func>),
}

impl AnyRef {
    pub fn null() -> AnyRef {
        AnyRef::Null
    }
}

impl fmt::Debug for AnyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnyRef::Null => write!(f, "null"),
            AnyRef::Rc(_) => write!(f, "anyref"),
            AnyRef::Func(_) => write!(f, "funcref"),
        }
    }
}

impl Into<Ref<Func>> for AnyRef {
    fn into(self) -> Ref<Func> {
        match self {
            AnyRef::Func(f) => f,
            AnyRef::Rc(_) => unimplemented!("try to unwrap?"),
            AnyRef::Null => panic!("null anyref"),
        }
    }
}

impl From<Ref<Func>> for AnyRef {
    fn from(r: Ref<Func>) -> AnyRef {
        AnyRef::Func(r)
    }
}

#[derive(Debug, Clone)]
pub enum Val {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    AnyRef(AnyRef),
    FuncRef(Ref<Func>),
}

impl Val {
    pub fn default() -> Val {
        Val::AnyRef(AnyRef::null())
    }

    pub fn r#type(&self) -> ValType {
        match self {
            Val::I32(_) => ValType::I32,
            Val::I64(_) => ValType::I64,
            Val::F32(_) => ValType::F32,
            Val::F64(_) => ValType::F64,
            Val::AnyRef(_) => ValType::AnyRef,
            Val::FuncRef(_) => ValType::FuncRef,
        }
    }

    pub(crate) unsafe fn write_value_to(&self, p: *mut i64) {
        match self {
            Val::I32(i) => ptr::write(p as *mut i32, *i),
            Val::I64(i) => ptr::write(p as *mut i64, *i),
            Val::F32(u) => ptr::write(p as *mut u32, *u),
            Val::F64(u) => ptr::write(p as *mut u64, *u),
            _ => unimplemented!("Val::write_value_to"),
        }
    }

    pub(crate) unsafe fn read_value_from(p: *const i64, ty: ir::Type) -> Val {
        match ty {
            ir::types::I32 => Val::I32(ptr::read(p as *const i32)),
            ir::types::I64 => Val::I64(ptr::read(p as *const i64)),
            ir::types::F32 => Val::F32(ptr::read(p as *const u32)),
            ir::types::F64 => Val::F64(ptr::read(p as *const u64)),
            _ => unimplemented!("Val::read_value_from"),
        }
    }

    pub fn from_f32_bits(v: u32) -> Val {
        Val::F32(v)
    }

    pub fn from_f64_bits(v: u64) -> Val {
        Val::F64(v)
    }
}

impl From<i32> for Val {
    fn from(val: i32) -> Val {
        Val::I32(val)
    }
}

impl From<i64> for Val {
    fn from(val: i64) -> Val {
        Val::I64(val)
    }
}

impl From<f32> for Val {
    fn from(val: f32) -> Val {
        Val::F32(val.to_bits())
    }
}

impl From<f64> for Val {
    fn from(val: f64) -> Val {
        Val::F64(val.to_bits())
    }
}

impl Into<i32> for Val {
    fn into(self) -> i32 {
        if let Val::I32(i) = self {
            i
        } else {
            panic!("Invalid conversion of {:?} to i32.", self);
        }
    }
}

impl Into<i64> for Val {
    fn into(self) -> i64 {
        if let Val::I64(i) = self {
            i
        } else {
            panic!("Invalid conversion of {:?} to i64.", self);
        }
    }
}

impl Into<f32> for Val {
    fn into(self) -> f32 {
        if let Val::F32(i) = self {
            RuntimeValue::F32(i).unwrap_f32()
        } else {
            panic!("Invalid conversion of {:?} to f32.", self);
        }
    }
}

impl Into<f64> for Val {
    fn into(self) -> f64 {
        if let Val::F64(i) = self {
            RuntimeValue::F64(i).unwrap_f64()
        } else {
            panic!("Invalid conversion of {:?} to f64.", self);
        }
    }
}

impl From<AnyRef> for Val {
    fn from(val: AnyRef) -> Val {
        match val {
            AnyRef::Func(f) => Val::FuncRef(f),
            _ => Val::AnyRef(val),
        }
    }
}

impl From<Ref<Func>> for Val {
    fn from(val: Ref<Func>) -> Val {
        Val::FuncRef(val)
    }
}

impl Into<AnyRef> for Val {
    fn into(self) -> AnyRef {
        match self {
            Val::AnyRef(r) => r,
            Val::FuncRef(f) => AnyRef::Func(f),
            _ => panic!("Invalid conversion of {:?} to anyref.", self),
        }
    }
}
