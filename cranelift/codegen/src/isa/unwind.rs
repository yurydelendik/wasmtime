//! Represents information relating to function unwinding.
#[cfg(feature = "enable-serde")]
use serde::{Deserialize, Serialize};

pub mod systemv;
pub mod winx64;

/// Represents unwind information for a single function.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub enum UnwindInfo {
    /// Windows x64 ABI unwind information.
    WindowsX64(winx64::UnwindInfo),
    /// System V ABI unwind information.
    SystemV(systemv::UnwindInfo),
}

pub(crate) mod input {
    use crate::binemit::CodeOffset;
    use alloc::vec::Vec;
        
    #[derive(Clone, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
    pub(crate) enum UnwindCode<Reg> {
        PushRegister {
            offset: CodeOffset,
            reg: Reg,
        },
        PopRegister {
            offset: CodeOffset,
            reg: Reg,
        },
        SaveXmm {
            offset: CodeOffset,
            reg: Reg,
            stack_offset: u32,
        },
        StackAlloc {
            offset: CodeOffset,
            size: u32,
        },
        StackDealloc {
            offset: CodeOffset,
            size: u32,
        },
        SetCfaRegister {
            offset: CodeOffset,
            reg: Reg,
        },
        RememberState {
            offset: CodeOffset,
        },
        RestoreState {
            offset: CodeOffset,
        },
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
    pub struct UnwindInfo<Reg> {
        pub(crate) prologue_size: CodeOffset,
        pub(crate) prologue_unwind_codes: Vec<UnwindCode<Reg>>,
        pub(crate) epilogues_unwind_codes: Vec<Vec<UnwindCode<Reg>>>,
        pub(crate) code_len: CodeOffset,
    }
}