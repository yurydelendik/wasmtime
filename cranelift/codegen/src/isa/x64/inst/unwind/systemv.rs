//! Unwind information for System V ABI (x86-64).

use crate::isa::unwind::systemv::{RegisterMappingError, UnwindInfo};
use crate::isa::x64::inst::{
    args::{AluRmiROpcode, Amode, RegMemImm, SyntheticAmode},
    regs, Inst,
};
use crate::machinst::UnwindInfoContext;
use crate::result::CodegenResult;
use alloc::vec::Vec;
use gimli::{write::CommonInformationEntry, Encoding, Format, Register, X86_64};
use regalloc::{Reg, RegClass};

/// Creates a new x86-64 common information entry (CIE).
pub fn create_cie() -> CommonInformationEntry {
    use gimli::write::CallFrameInstruction;

    let mut entry = CommonInformationEntry::new(
        Encoding {
            address_size: 8,
            format: Format::Dwarf32,
            version: 1,
        },
        1,  // Code alignment factor
        -8, // Data alignment factor
        X86_64::RA,
    );

    // Every frame will start with the call frame address (CFA) at RSP+8
    // It is +8 to account for the push of the return address by the call instruction
    entry.add_instruction(CallFrameInstruction::Cfa(X86_64::RSP, 8));

    // Every frame will start with the return address at RSP (CFA-8 = RSP+8-8 = RSP)
    entry.add_instruction(CallFrameInstruction::Offset(X86_64::RA, -8));

    entry
}

/// Map Cranelift registers to their corresponding Gimli registers.
pub fn map_reg(reg: Reg) -> Result<Register, RegisterMappingError> {
    // Mapping from https://github.com/bytecodealliance/cranelift/pull/902 by @iximeow
    const X86_GP_REG_MAP: [gimli::Register; 16] = [
        X86_64::RAX,
        X86_64::RCX,
        X86_64::RDX,
        X86_64::RBX,
        X86_64::RSP,
        X86_64::RBP,
        X86_64::RSI,
        X86_64::RDI,
        X86_64::R8,
        X86_64::R9,
        X86_64::R10,
        X86_64::R11,
        X86_64::R12,
        X86_64::R13,
        X86_64::R14,
        X86_64::R15,
    ];
    const X86_XMM_REG_MAP: [gimli::Register; 16] = [
        X86_64::XMM0,
        X86_64::XMM1,
        X86_64::XMM2,
        X86_64::XMM3,
        X86_64::XMM4,
        X86_64::XMM5,
        X86_64::XMM6,
        X86_64::XMM7,
        X86_64::XMM8,
        X86_64::XMM9,
        X86_64::XMM10,
        X86_64::XMM11,
        X86_64::XMM12,
        X86_64::XMM13,
        X86_64::XMM14,
        X86_64::XMM15,
    ];

    match reg.get_class() {
        RegClass::I64 => {
            // x86 GP registers have a weird mapping to DWARF registers, so we use a
            // lookup table.
            Ok(X86_GP_REG_MAP[reg.get_hw_encoding() as usize])
        }
        RegClass::V128 => Ok(X86_XMM_REG_MAP[reg.get_hw_encoding() as usize]),
        _ => Err(RegisterMappingError::UnsupportedRegisterBank("class?")),
    }
}

pub(crate) fn create_unwind_info(
    context: UnwindInfoContext<Inst>,
    word_size: u8,
) -> CodegenResult<Option<UnwindInfo>> {
    use crate::isa::unwind::input::{self, UnwindCode};
    let mut codes = Vec::new();

    for i in context.prologue.clone() {
        let i = i as usize;
        let inst = &context.insts[i];
        let offset = context.insts_layout[i];

        match inst {
            Inst::Push64 {
                src: RegMemImm::Reg { reg },
            } => {
                codes.push((
                    offset,
                    UnwindCode::StackAlloc {
                        size: word_size.into(),
                    },
                ));
                codes.push((
                    offset,
                    UnwindCode::SaveRegister {
                        reg: *reg,
                        stack_offset: 0,
                    },
                ));
            }
            Inst::MovRR { src, dst, .. } => {
                if *src == regs::rsp() {
                    codes.push((offset, UnwindCode::SetFramePointer { reg: dst.to_reg() }));
                }
            }
            Inst::AluRmiR {
                is_64: true,
                op: AluRmiROpcode::Sub,
                src: RegMemImm::Imm { simm32 },
                dst,
                ..
            } if dst.to_reg() == regs::rsp() => {
                let imm = *simm32;
                codes.push((offset, UnwindCode::StackAlloc { size: imm }));
            }
            Inst::MovRM {
                src,
                dst: SyntheticAmode::Real(Amode::ImmReg { simm32, base }),
                ..
            } if *base == regs::rsp() => {
                // `mov reg, imm(rsp)`
                let imm = *simm32;
                codes.push((
                    offset,
                    UnwindCode::SaveRegister {
                        reg: *src,
                        stack_offset: imm,
                    },
                ));
            }
            Inst::AluRmiR {
                is_64: true,
                op: AluRmiROpcode::Add,
                src: RegMemImm::Imm { simm32 },
                dst,
                ..
            } if dst.to_reg() == regs::rsp() => {
                let imm = *simm32;
                codes.push((offset, UnwindCode::StackDealloc { size: imm }));
            }
            _ => {}
        }
    }

    let last_epilogue_end = context.len;
    let epilogues_unwind_codes = context
        .epilogues
        .iter()
        .map(|epilogue| {
            let end = epilogue.end as usize - 1;
            let end_offset = context.insts_layout[end];
            if end_offset == last_epilogue_end {
                // Do not remember/restore for very last epilogue.
                return vec![];
            }

            let start = epilogue.start as usize;
            let offset = context.insts_layout[start];
            vec![
                (offset, UnwindCode::RememberState),
                (end_offset, UnwindCode::RestoreState),
            ]
        })
        .collect();

    let prologue_size = context.insts_layout[context.prologue.end as usize];
    let unwind = input::UnwindInfo {
        prologue_size,
        prologue_unwind_codes: codes,
        epilogues_unwind_codes,
        function_size: context.len,
        word_size,
    };

    struct RegisterMapper;
    impl crate::isa::unwind::systemv::RegisterMapper<Reg> for RegisterMapper {
        fn map(&self, reg: Reg) -> Result<u16, RegisterMappingError> {
            Ok(map_reg(reg)?.0)
        }
        fn rsp(&self) -> u16 {
            map_reg(regs::rsp()).unwrap().0
        }
    }
    let map = RegisterMapper;

    Ok(Some(UnwindInfo::build(unwind, &map)?))
}

#[cfg(test)]
mod tests {
    use crate::cursor::{Cursor, FuncCursor};
    use crate::ir::{
        types, AbiParam, ExternalName, Function, InstBuilder, Signature, StackSlotData,
        StackSlotKind,
    };
    use crate::isa::{lookup, CallConv};
    use crate::settings::{builder, Flags};
    use crate::Context;
    use gimli::write::Address;
    use std::str::FromStr;
    use target_lexicon::triple;

    #[test]
    fn test_simple_func() {
        let isa = lookup(triple!("x86_64"))
            .expect("expect x86 ISA")
            .finish(Flags::new(builder()));

        let mut context = Context::for_function(create_function(
            CallConv::SystemV,
            Some(StackSlotData::new(StackSlotKind::ExplicitSlot, 64)),
        ));

        context.compile(&*isa).expect("expected compilation");

        let fde = match context
            .create_unwind_info(isa.as_ref())
            .expect("can create unwind info")
        {
            Some(crate::isa::unwind::UnwindInfo::SystemV(info)) => {
                info.to_fde(Address::Constant(1234))
            }
            _ => panic!("expected unwind information"),
        };

        assert_eq!(format!("{:?}", fde), "FrameDescriptionEntry { address: Constant(1234), length: 13, lsda: None, instructions: [(1, CfaOffset(16)), (1, Offset(Register(6), -16)), (4, CfaRegister(Register(6)))] }");
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

    #[test]
    fn test_multi_return_func() {
        let isa = lookup(triple!("x86_64"))
            .expect("expect x86 ISA")
            .finish(Flags::new(builder()));

        let mut context = Context::for_function(create_multi_return_function(CallConv::SystemV));

        context.compile(&*isa).expect("expected compilation");

        let fde = match context
            .create_unwind_info(isa.as_ref())
            .expect("can create unwind info")
        {
            Some(crate::isa::unwind::UnwindInfo::SystemV(info)) => {
                info.to_fde(Address::Constant(4321))
            }
            _ => panic!("expected unwind information"),
        };

        assert_eq!(format!("{:?}", fde), "FrameDescriptionEntry { address: Constant(4321), length: 23, lsda: None, instructions: [(1, CfaOffset(16)), (1, Offset(Register(6), -16)), (4, CfaRegister(Register(6))), (16, RememberState), (18, RestoreState)] }");
    }

    fn create_multi_return_function(call_conv: CallConv) -> Function {
        let mut sig = Signature::new(call_conv);
        sig.params.push(AbiParam::new(types::I32));
        let mut func = Function::with_name_signature(ExternalName::user(0, 0), sig);

        let block0 = func.dfg.make_block();
        let v0 = func.dfg.append_block_param(block0, types::I32);
        let block1 = func.dfg.make_block();
        let block2 = func.dfg.make_block();

        let mut pos = FuncCursor::new(&mut func);
        pos.insert_block(block0);
        pos.ins().brnz(v0, block2, &[]);
        pos.ins().jump(block1, &[]);

        pos.insert_block(block1);
        pos.ins().return_(&[]);

        pos.insert_block(block2);
        pos.ins().return_(&[]);

        func
    }
}
