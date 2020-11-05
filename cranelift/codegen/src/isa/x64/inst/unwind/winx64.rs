//! Unwind information for Windows x64 ABI.

use crate::isa::unwind::input;
use crate::isa::unwind::winx64::UnwindInfo;
use crate::result::CodegenResult;
use regalloc::{Reg, RegClass};

pub(crate) fn create_unwind_info(
    unwind: input::UnwindInfo<Reg>,
) -> CodegenResult<Option<UnwindInfo>> {
    Ok(Some(UnwindInfo::build::<_, RegisterMapper>(unwind)?))
}

struct RegisterMapper;

impl crate::isa::unwind::winx64::RegisterMapper<Reg> for RegisterMapper {
    fn map(reg: Reg) -> crate::isa::unwind::winx64::MappedRegister {
        use crate::isa::unwind::winx64::MappedRegister;
        match reg.get_class() {
            RegClass::I64 => MappedRegister::Int(reg.get_hw_encoding()),
            RegClass::V128 => MappedRegister::Xmm(reg.get_hw_encoding()),
            _ => panic!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cursor::{Cursor, FuncCursor};
    use crate::ir::{ExternalName, Function, InstBuilder, Signature, StackSlotData, StackSlotKind};
    use crate::isa::unwind::winx64::{UnwindCode, UnwindInfo};
    use crate::isa::x64::inst::regs;
    use crate::isa::{lookup, CallConv};
    use crate::settings::{builder, Flags};
    use crate::Context;
    use std::str::FromStr;
    use target_lexicon::triple;

    #[test]
    #[cfg_attr(feature = "x64", should_panic)] // TODO #2372
    fn test_small_alloc() {
        let isa = lookup(triple!("x86_64"))
            .expect("expect x86 ISA")
            .finish(Flags::new(builder()));

        let mut context = Context::for_function(create_function(
            CallConv::WindowsFastcall,
            Some(StackSlotData::new(StackSlotKind::ExplicitSlot, 64)),
        ));

        context.compile(&*isa).expect("expected compilation");

        let unwind = match context
            .create_unwind_info(isa.as_ref())
            .expect("can create unwind info")
            .expect("expected unwind info")
        {
            crate::isa::unwind::UnwindInfo::WindowsX64(i) => i,
            _ => panic!("expected UnwindInfo::WindowsX64"),
        };

        assert_eq!(
            unwind,
            UnwindInfo {
                flags: 0,
                prologue_size: 9,
                frame_register: None,
                frame_register_offset: 0,
                unwind_codes: vec![
                    UnwindCode::PushRegister {
                        offset: 2,
                        reg: regs::rbp().get_hw_encoding()
                    },
                    UnwindCode::StackAlloc {
                        offset: 9,
                        size: 64
                    }
                ]
            }
        );

        assert_eq!(unwind.emit_size(), 8);

        let mut buf = [0u8; 8];
        unwind.emit(&mut buf);

        assert_eq!(
            buf,
            [
                0x01, // Version and flags (version 1, no flags)
                0x09, // Prologue size
                0x02, // Unwind code count (1 for stack alloc, 1 for push reg)
                0x00, // Frame register + offset (no frame register)
                0x09, // Prolog offset
                0x72, // Operation 2 (small stack alloc), size = 0xB slots (e.g. (0x7 * 8) + 8 = 64 bytes)
                0x02, // Prolog offset
                0x50, // Operation 0 (save nonvolatile register), reg = 5 (RBP)
            ]
        );
    }

    #[test]
    #[cfg_attr(feature = "x64", should_panic)] // TODO #2372
    fn test_medium_alloc() {
        let isa = lookup(triple!("x86_64"))
            .expect("expect x86 ISA")
            .finish(Flags::new(builder()));

        let mut context = Context::for_function(create_function(
            CallConv::WindowsFastcall,
            Some(StackSlotData::new(StackSlotKind::ExplicitSlot, 10000)),
        ));

        context.compile(&*isa).expect("expected compilation");

        let unwind = match context
            .create_unwind_info(isa.as_ref())
            .expect("can create unwind info")
            .expect("expected unwind info")
        {
            crate::isa::unwind::UnwindInfo::WindowsX64(i) => i,
            _ => panic!("expected UnwindInfo::WindowsX64"),
        };

        assert_eq!(
            unwind,
            UnwindInfo {
                flags: 0,
                prologue_size: 27,
                frame_register: None,
                frame_register_offset: 0,
                unwind_codes: vec![
                    UnwindCode::PushRegister {
                        offset: 2,
                        reg: regs::rbp().get_hw_encoding()
                    },
                    UnwindCode::StackAlloc {
                        offset: 27,
                        size: 10000
                    }
                ]
            }
        );

        assert_eq!(unwind.emit_size(), 12);

        let mut buf = [0u8; 12];
        unwind.emit(&mut buf);

        assert_eq!(
            buf,
            [
                0x01, // Version and flags (version 1, no flags)
                0x1B, // Prologue size
                0x03, // Unwind code count (2 for stack alloc, 1 for push reg)
                0x00, // Frame register + offset (no frame register)
                0x1B, // Prolog offset
                0x01, // Operation 1 (large stack alloc), size is scaled 16-bits (info = 0)
                0xE2, // Low size byte
                0x04, // High size byte (e.g. 0x04E2 * 8 = 10000 bytes)
                0x02, // Prolog offset
                0x50, // Operation 0 (push nonvolatile register), reg = 5 (RBP)
                0x00, // Padding
                0x00, // Padding
            ]
        );
    }

    #[test]
    #[cfg_attr(feature = "x64", should_panic)] // TODO #2372
    fn test_large_alloc() {
        let isa = lookup(triple!("x86_64"))
            .expect("expect x86 ISA")
            .finish(Flags::new(builder()));

        let mut context = Context::for_function(create_function(
            CallConv::WindowsFastcall,
            Some(StackSlotData::new(StackSlotKind::ExplicitSlot, 1000000)),
        ));

        context.compile(&*isa).expect("expected compilation");

        let unwind = match context
            .create_unwind_info(isa.as_ref())
            .expect("can create unwind info")
            .expect("expected unwind info")
        {
            crate::isa::unwind::UnwindInfo::WindowsX64(i) => i,
            _ => panic!("expected UnwindInfo::WindowsX64"),
        };

        assert_eq!(
            unwind,
            UnwindInfo {
                flags: 0,
                prologue_size: 27,
                frame_register: None,
                frame_register_offset: 0,
                unwind_codes: vec![
                    UnwindCode::PushRegister {
                        offset: 2,
                        reg: regs::rbp().get_hw_encoding()
                    },
                    UnwindCode::StackAlloc {
                        offset: 27,
                        size: 1000000
                    }
                ]
            }
        );

        assert_eq!(unwind.emit_size(), 12);

        let mut buf = [0u8; 12];
        unwind.emit(&mut buf);

        assert_eq!(
            buf,
            [
                0x01, // Version and flags (version 1, no flags)
                0x1B, // Prologue size
                0x04, // Unwind code count (3 for stack alloc, 1 for push reg)
                0x00, // Frame register + offset (no frame register)
                0x1B, // Prolog offset
                0x11, // Operation 1 (large stack alloc), size is unscaled 32-bits (info = 1)
                0x40, // Byte 1 of size
                0x42, // Byte 2 of size
                0x0F, // Byte 3 of size
                0x00, // Byte 4 of size (size is 0xF4240 = 1000000 bytes)
                0x02, // Prolog offset
                0x50, // Operation 0 (push nonvolatile register), reg = 5 (RBP)
            ]
        );
    }

    fn create_function(call_conv: CallConv, stack_slot: Option<StackSlotData>) -> Function {
        let mut func =
            Function::with_name_signature(ExternalName::user(0, 0), Signature::new(call_conv));

        let block0 = func.dfg.make_block();
        let mut pos = FuncCursor::new(&mut func);
        pos.insert_block(block0);
        pos.ins().return_(&[]);

        if let Some(stack_slot) = stack_slot {
            func.stack_slots.push(stack_slot);
        }

        func
    }
}
