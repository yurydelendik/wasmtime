use crate::DebugInfoData;
use cranelift_codegen::isa::TargetFrontendConfig;
use failure::Error;

use gimli;

use gimli::{
    DebugAbbrev, DebugAddr, DebugAddrBase, DebugLine, DebugStr, LocationLists, RangeLists,
};

use gimli::write;

pub use address_transform::AddressTransform;
pub use data::{
    FunctionAddressMap, InstructionAddressMap, ModuleAddressMap, ModuleVmctxInfo, ValueLabelsRanges,
};

use unit::clone_unit;

mod address_transform;
mod attr;
mod data;
mod expression;
mod line_program;
mod range_info_builder;
mod unit;

pub(crate) trait Reader: gimli::Reader<Offset = usize> {}

impl<'input, Endian> Reader for gimli::EndianSlice<'input, Endian> where Endian: gimli::Endianity {}

#[derive(Fail, Debug)]
#[fail(display = "Debug info transform error: {}", _0)]
pub struct TransformError(&'static str);

pub(crate) struct DebugInputContext<'a, R>
where
    R: Reader,
{
    debug_abbrev: &'a DebugAbbrev<R>,
    debug_str: &'a DebugStr<R>,
    debug_line: &'a DebugLine<R>,
    debug_addr: &'a DebugAddr<R>,
    debug_addr_base: DebugAddrBase<R::Offset>,
    rnglists: &'a RangeLists<R>,
    loclists: &'a LocationLists<R>,
}

pub fn transform_dwarf(
    target_config: &TargetFrontendConfig,
    di: &DebugInfoData,
    at: &ModuleAddressMap,
    vmctx_info: &ModuleVmctxInfo,
    ranges: &ValueLabelsRanges,
) -> Result<write::Dwarf, Error> {
    let context = DebugInputContext {
        debug_abbrev: &di.dwarf.debug_abbrev,
        debug_str: &di.dwarf.debug_str,
        debug_line: &di.dwarf.debug_line,
        debug_addr: &di.dwarf.debug_addr,
        debug_addr_base: DebugAddrBase(0),
        rnglists: &di.dwarf.ranges,
        loclists: &di.dwarf.locations,
    };

    let out_encoding = gimli::Encoding {
        format: gimli::Format::Dwarf32,
        // TODO: this should be configurable
        // macOS doesn't seem to support DWARF > 3
        version: 3,
        address_size: target_config.pointer_bytes(),
    };

    let addr_tr = AddressTransform::new(at, &di.wasm_file);

    let mut out_strings = write::StringTable::default();
    let mut out_units = write::UnitTable::default();

    let out_line_strings = write::LineStringTable::default();

    let mut iter = di.dwarf.debug_info.units();
    while let Some(ref unit) = iter.next().unwrap_or(None) {
        clone_unit(
            unit,
            &context,
            &addr_tr,
            &ranges,
            &out_encoding,
            &vmctx_info,
            &mut out_units,
            &mut out_strings,
        )?;
    }

    Ok(write::Dwarf {
        units: out_units,
        line_programs: vec![],
        line_strings: out_line_strings,
        strings: out_strings,
    })
}
