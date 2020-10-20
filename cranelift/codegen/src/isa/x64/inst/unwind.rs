use super::Inst;
use crate::isa::unwind::UnwindInfo;
use crate::machinst::{UnwindInfoContext, UnwindInfoGenerator, UnwindInfoKind};
use crate::result::CodegenResult;

#[cfg(feature = "unwind")]
pub use self::systemv::create_cie;
#[cfg(feature = "unwind")]
use super::regs;

#[cfg(feature = "unwind")]
mod systemv;

pub struct X64UnwindInfo;

impl UnwindInfoGenerator<Inst> for X64UnwindInfo {
    #[allow(unused_variables)]
    fn create_unwind_info(
        context: UnwindInfoContext<Inst>,
        kind: UnwindInfoKind,
    ) -> CodegenResult<Option<UnwindInfo>> {
        // Assumption: RBP is being used as the frame pointer for both calling conventions
        // In the future, we should be omitting frame pointer as an optimization, so this will change
        Ok(match kind {
            #[cfg(feature = "unwind")]
            UnwindInfoKind::SystemV => {
                systemv::create_unwind_info(context, Some(regs::rbp()))?.map(UnwindInfo::SystemV)
            }
            #[cfg(feature = "unwind")]
            UnwindInfoKind::Windows => {
                //TODO super::unwind::winx64::create_unwind_info(func, isa)?.map(|u| UnwindInfo::WindowsX64(u))
                panic!();
            }
            UnwindInfoKind::None => None,
        })
    }
}
