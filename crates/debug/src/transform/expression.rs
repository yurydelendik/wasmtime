use super::address_transform::AddressTransform;
use anyhow::{Context, Error, Result};
use gimli::{self, write, Expression, Operation, Reader, ReaderOffset, X86_64};
use more_asserts::{assert_le, assert_lt};
use std::collections::{HashMap, HashSet};
use wasmtime_environ::entity::EntityRef;
use wasmtime_environ::ir::{StackSlots, ValueLabel, ValueLabelsRanges, ValueLoc};
use wasmtime_environ::isa::TargetIsa;
use wasmtime_environ::wasm::{get_vmctx_value_label, DefinedFuncIndex};
use wasmtime_environ::ModuleMemoryOffset;

#[derive(Debug)]
pub struct FunctionFrameInfo<'a> {
    pub value_ranges: &'a ValueLabelsRanges,
    pub memory_offset: ModuleMemoryOffset,
    pub stack_slots: &'a StackSlots,
}

impl<'a> FunctionFrameInfo<'a> {
    fn vmctx_memory_offset(&self) -> Option<i64> {
        match self.memory_offset {
            ModuleMemoryOffset::Defined(x) => Some(x as i64),
            ModuleMemoryOffset::Imported(_) => {
                // TODO implement memory offset for imported memory
                None
            }
            ModuleMemoryOffset::None => None,
        }
    }
}

struct ExpressionWriter(write::EndianVec<gimli::RunTimeEndian>);

impl ExpressionWriter {
    pub fn new() -> Self {
        let endian = gimli::RunTimeEndian::Little;
        let writer = write::EndianVec::new(endian);
        ExpressionWriter(writer)
    }

    pub fn write_op(&mut self, op: gimli::DwOp) -> write::Result<()> {
        self.write_u8(op.0 as u8)
    }

    pub fn write_op_reg(&mut self, reg: u16) -> write::Result<()> {
        if reg < 32 {
            self.write_u8(gimli::constants::DW_OP_reg0.0 as u8 + reg as u8)
        } else {
            self.write_op(gimli::constants::DW_OP_regx)?;
            self.write_uleb128(reg.into())
        }
    }

    pub fn write_op_breg(&mut self, reg: u16) -> write::Result<()> {
        if reg < 32 {
            self.write_u8(gimli::constants::DW_OP_breg0.0 as u8 + reg as u8)
        } else {
            self.write_op(gimli::constants::DW_OP_bregx)?;
            self.write_uleb128(reg.into())
        }
    }

    pub fn write_u8(&mut self, b: u8) -> write::Result<()> {
        write::Writer::write_u8(&mut self.0, b)
    }

    pub fn write_u32(&mut self, b: u32) -> write::Result<()> {
        write::Writer::write_u32(&mut self.0, b)
    }

    pub fn write_uleb128(&mut self, i: u64) -> write::Result<()> {
        write::Writer::write_uleb128(&mut self.0, i)
    }

    pub fn write_sleb128(&mut self, i: i64) -> write::Result<()> {
        write::Writer::write_sleb128(&mut self.0, i)
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.0.into_vec()
    }
}

#[derive(Debug, Clone, PartialEq)]
enum CompiledExpressionPart {
    // Untranslated DWARF expression.
    Code(Vec<u8>),
    // The wasm-local DWARF operator. The label points to `ValueLabel`.
    // The trailing field denotes that the operator was last in sequence,
    // and it is the DWARF location (not a pointer).
    Local { label: ValueLabel, trailing: bool },
    // Dereference is needed.
    Deref,
    // Jumping in the expression.
    Jump { target: i16, conditionally: bool },
    // Floating landing pad.
    LandingPad { original_pos: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledExpression {
    parts: Vec<CompiledExpressionPart>,
    need_deref: bool,
    jump_arcs: HashMap<usize, usize>,
}

impl CompiledExpression {
    pub fn vmctx() -> CompiledExpression {
        CompiledExpression::from_label(get_vmctx_value_label())
    }

    pub fn from_label(label: ValueLabel) -> CompiledExpression {
        CompiledExpression {
            parts: vec![CompiledExpressionPart::Local {
                label,
                trailing: true,
            }],
            need_deref: false,
            jump_arcs: HashMap::new(),
        }
    }
}

const X86_64_STACK_OFFSET: i64 = 16;

fn translate_loc(
    loc: ValueLoc,
    frame_info: Option<&FunctionFrameInfo>,
    isa: &dyn TargetIsa,
    add_stack_value: bool,
) -> Result<Option<Vec<u8>>> {
    Ok(match loc {
        ValueLoc::Reg(reg) if add_stack_value => {
            let machine_reg = isa.map_dwarf_register(reg)?;
            let mut writer = ExpressionWriter::new();
            writer.write_op_reg(machine_reg)?;
            Some(writer.into_vec())
        }
        ValueLoc::Reg(reg) => {
            assert!(!add_stack_value);
            let machine_reg = isa.map_dwarf_register(reg)?;
            let mut writer = ExpressionWriter::new();
            writer.write_op_breg(machine_reg)?;
            writer.write_sleb128(0)?;
            Some(writer.into_vec())
        }
        ValueLoc::Stack(ss) => {
            if let Some(frame_info) = frame_info {
                if let Some(ss_offset) = frame_info.stack_slots[ss].offset {
                    let mut writer = ExpressionWriter::new();
                    writer.write_op_breg(X86_64::RBP.0)?;
                    writer.write_sleb128(ss_offset as i64 + X86_64_STACK_OFFSET)?;
                    if !add_stack_value {
                        writer.write_op(gimli::constants::DW_OP_deref)?;
                    }
                    return Ok(Some(writer.into_vec()));
                }
            }
            None
        }
        _ => None,
    })
}

fn append_memory_deref(
    buf: &mut Vec<u8>,
    frame_info: &FunctionFrameInfo,
    vmctx_loc: ValueLoc,
    isa: &dyn TargetIsa,
) -> Result<bool> {
    let mut writer = ExpressionWriter::new();
    // FIXME for imported memory
    match vmctx_loc {
        ValueLoc::Reg(vmctx_reg) => {
            let reg = isa.map_dwarf_register(vmctx_reg)? as u8;
            writer.write_u8(gimli::constants::DW_OP_breg0.0 + reg)?;
            let memory_offset = match frame_info.vmctx_memory_offset() {
                Some(offset) => offset,
                None => {
                    return Ok(false);
                }
            };
            writer.write_sleb128(memory_offset)?;
        }
        ValueLoc::Stack(ss) => {
            if let Some(ss_offset) = frame_info.stack_slots[ss].offset {
                writer.write_op_breg(X86_64::RBP.0)?;
                writer.write_sleb128(ss_offset as i64 + X86_64_STACK_OFFSET)?;
                writer.write_op(gimli::constants::DW_OP_deref)?;
                writer.write_op(gimli::constants::DW_OP_consts)?;
                let memory_offset = match frame_info.vmctx_memory_offset() {
                    Some(offset) => offset,
                    None => {
                        return Ok(false);
                    }
                };
                writer.write_sleb128(memory_offset)?;
                writer.write_op(gimli::constants::DW_OP_plus)?;
            } else {
                return Ok(false);
            }
        }
        _ => {
            return Ok(false);
        }
    }
    writer.write_op(gimli::constants::DW_OP_deref)?;
    writer.write_op(gimli::constants::DW_OP_swap)?;
    writer.write_op(gimli::constants::DW_OP_const4u)?;
    writer.write_u32(0xffff_ffff)?;
    writer.write_op(gimli::constants::DW_OP_and)?;
    writer.write_op(gimli::constants::DW_OP_plus)?;
    buf.extend(writer.into_vec());
    Ok(true)
}

impl CompiledExpression {
    pub fn is_simple(&self) -> bool {
        if let [CompiledExpressionPart::Code(_)] = self.parts.as_slice() {
            true
        } else {
            self.parts.is_empty()
        }
    }

    pub fn build(&self) -> Option<write::Expression> {
        if let [CompiledExpressionPart::Code(code)] = self.parts.as_slice() {
            return Some(write::Expression::raw(code.to_vec()));
        }
        // locals found, not supported
        None
    }

    pub fn build_with_locals<'a>(
        &'a self,
        scope: &'a [(u64, u64)], // wasm ranges
        addr_tr: &'a AddressTransform,
        frame_info: Option<&'a FunctionFrameInfo>,
        isa: &'a dyn TargetIsa,
    ) -> impl Iterator<Item = Result<(write::Address, u64, write::Expression)>> + 'a {
        enum BuildWithLocalsResult<'a> {
            Empty,
            Simple(
                Box<dyn Iterator<Item = (write::Address, u64)> + 'a>,
                Vec<u8>,
            ),
            Ranges(
                Box<dyn Iterator<Item = Result<(DefinedFuncIndex, usize, usize, Vec<u8>)>> + 'a>,
            ),
        }
        impl Iterator for BuildWithLocalsResult<'_> {
            type Item = Result<(write::Address, u64, write::Expression)>;
            fn next(&mut self) -> Option<Self::Item> {
                match self {
                    BuildWithLocalsResult::Empty => None,
                    BuildWithLocalsResult::Simple(it, code) => it
                        .next()
                        .map(|(addr, len)| Ok((addr, len, write::Expression::raw(code.to_vec())))),
                    BuildWithLocalsResult::Ranges(it) => it.next().map(|r| {
                        r.map(|(func_index, start, end, code_buf)| {
                            (
                                write::Address::Symbol {
                                    symbol: func_index.index(),
                                    addend: start as i64,
                                },
                                (end - start) as u64,
                                write::Expression::raw(code_buf),
                            )
                        })
                    }),
                }
            }
        }

        if scope.is_empty() {
            return BuildWithLocalsResult::Empty;
        }

        // If it a simple DWARF code, no need in locals processing. Just translate
        // the scope ranges.
        if let [CompiledExpressionPart::Code(code)] = self.parts.as_slice() {
            return BuildWithLocalsResult::Simple(
                Box::new(scope.iter().flat_map(move |(wasm_start, wasm_end)| {
                    addr_tr.translate_ranges(*wasm_start, *wasm_end)
                })),
                code.clone(),
            );
        }

        let vmctx_label = get_vmctx_value_label();

        // Some locals are present, preparing and divided ranges based on the scope
        // and frame_info data.
        let mut ranges_builder = ValueLabelRangesBuilder::new(scope, addr_tr, frame_info);
        for p in self.parts.iter() {
            match p {
                CompiledExpressionPart::Code(_)
                | CompiledExpressionPart::Jump { .. }
                | CompiledExpressionPart::LandingPad { .. } => (),
                CompiledExpressionPart::Local { label, .. } => ranges_builder.process_label(*label),
                CompiledExpressionPart::Deref => ranges_builder.process_label(vmctx_label),
            }
        }
        if self.need_deref {
            ranges_builder.process_label(vmctx_label);
        }
        let ranges = ranges_builder.into_ranges();
        let mut old_to_new: HashMap<usize, usize> = HashMap::new();

        return BuildWithLocalsResult::Ranges(Box::new(
            ranges
                .into_iter()
                .map(
                    move |CachedValueLabelRange {
                              func_index,
                              start,
                              end,
                              label_location,
                          }| {
                        // build expression
                        let mut code_buf = Vec::new();
                        macro_rules! deref {
                            () => {
                                if let (Some(vmctx_loc), Some(frame_info)) =
                                    (label_location.get(&vmctx_label), frame_info)
                                {
                                    if !append_memory_deref(
                                        &mut code_buf,
                                        frame_info,
                                        *vmctx_loc,
                                        isa,
                                    )? {
                                        return Ok(None);
                                    }
                                } else {
                                    return Ok(None);
                                };
                            };
                        }
                        for part in &self.parts {
                            match part {
                                CompiledExpressionPart::Code(c) => {
                                    for (old, new) in old_to_new.clone() {
                                        if new == code_buf.len() {
                                            for i in 1..c.len() {
                                                old_to_new.insert(old + i, new + i);
                                            }
                                            break;
                                        }
                                    }
                                    code_buf.extend_from_slice(c.as_slice())
                                }
                                CompiledExpressionPart::LandingPad { original_pos } => {
                                    let new_pos = code_buf.len();
                                    old_to_new.insert(*original_pos, new_pos);
                                }
                                CompiledExpressionPart::Jump { conditionally, .. } => {
                                    code_buf.push(
                                        match conditionally {
                                            true => gimli::constants::DW_OP_bra,
                                            false => gimli::constants::DW_OP_skip,
                                        }
                                        .0 as u8,
                                    );
                                    code_buf.push(!0);
                                    code_buf.push(!0) // these will be relocated below
                                }
                                CompiledExpressionPart::Local { label, trailing } => {
                                    let loc =
                                        *label_location.get(&label).context("label_location")?;
                                    if let Some(expr) =
                                        translate_loc(loc, frame_info, isa, *trailing)?
                                    {
                                        code_buf.extend_from_slice(&expr)
                                    } else {
                                        return Ok(None);
                                    }
                                }
                                CompiledExpressionPart::Deref => deref!(),
                            }
                        }
                        if self.need_deref {
                            deref!();
                        }
                        for (from, to) in self.jump_arcs.clone() {
                            // relocate jump targets
                            let new_from = old_to_new[&from];
                            let new_to = old_to_new[&to];
                            let new_diff = new_to as i32 - new_from as i32;
                            code_buf[new_from - 2] = (new_diff & 0xFF) as u8;
                            code_buf[new_from - 1] = (new_diff >> (8 as u16)) as u8;
                            // FIXME: use encoding?
                        }

                        Ok(Some((func_index, start, end, code_buf)))
                    },
                )
                .filter_map(Result::transpose),
        ));
    }
}

fn is_old_expression_format(buf: &[u8]) -> bool {
    // Heuristic to detect old variable expression format without DW_OP_fbreg:
    // DW_OP_plus_uconst op must be present, but not DW_OP_fbreg.
    if buf.contains(&(gimli::constants::DW_OP_fbreg.0 as u8)) {
        // Stop check if DW_OP_fbreg exist.
        return false;
    }
    buf.contains(&(gimli::constants::DW_OP_plus_uconst.0 as u8))
}

pub fn compile_expression<R>(
    expr: &Expression<R>,
    encoding: gimli::Encoding,
    frame_base: Option<&CompiledExpression>,
) -> Result<Option<CompiledExpression>, Error>
where
    R: Reader,
{
    let mut jump_arcs: HashMap<usize, usize> = HashMap::new();
    let mut pc = expr.0.clone();

    let mut unread_bytes;
    let buf = expr.0.to_slice()?;
    let mut parts = Vec::new();
    macro_rules! push {
        ($part:expr) => {{
            let part = $part;
            if let (CompiledExpressionPart::Code(cc2), Some(CompiledExpressionPart::Code(cc1))) =
                (&part, parts.last())
            {
                let mut combined = cc1.clone();
                parts.pop();
                combined.extend_from_slice(cc2);
                parts.push(CompiledExpressionPart::Code(combined));
            } else {
                parts.push(CompiledExpressionPart::LandingPad {
                    original_pos: (expr.0.len().into_u64() - unread_bytes) as usize,
                });
                parts.push(part)
            }
        }};
    }
    let mut need_deref = false;
    if is_old_expression_format(&buf) && frame_base.is_some() {
        // Still supporting old DWARF variable expressions without fbreg.
        parts.extend_from_slice(&frame_base.unwrap().parts);
        if let Some(CompiledExpressionPart::Local { trailing, .. }) = parts.last_mut() {
            *trailing = false;
        }
        need_deref = frame_base.unwrap().need_deref;
    }
    let mut code_chunk = Vec::new();
    macro_rules! flush_code_chunk {
        () => {
            if !code_chunk.is_empty() {
                let corr = code_chunk.len().into_u64();
                unread_bytes += corr;
                push!(CompiledExpressionPart::Code(code_chunk));
                unread_bytes -= corr;
                code_chunk = Vec::new();
            }
        };
    };
    while !pc.is_empty() {
        unread_bytes = pc.len().into_u64();
        let next = buf[pc.offset_from(&expr.0).into_u64() as usize];
        need_deref = true;
        if next == 0xED {
            // WebAssembly DWARF extension
            pc.read_u8()?;
            let ty = pc.read_uleb128()?;
            // Supporting only wasm locals.
            if ty != 0 {
                // TODO support wasm globals?
                return Ok(None);
            }
            let index = pc.read_sleb128()?;
            flush_code_chunk!();
            let label = ValueLabel::from_u32(index as u32);
            push!(CompiledExpressionPart::Local {
                label,
                trailing: false,
            });
        } else {
            let pos = pc.offset_from(&expr.0).into_u64() as usize;
            let op = Operation::parse(&mut pc, encoding)?;
            match op {
                Operation::FrameOffset { offset } => {
                    // Expand DW_OP_fbreg into frame location and DW_OP_plus_uconst.
                    if frame_base.is_some() {
                        // Add frame base expressions.
                        flush_code_chunk!();
                        parts.extend_from_slice(&frame_base.unwrap().parts);
                    }
                    if let Some(CompiledExpressionPart::Local { trailing, .. }) = parts.last_mut() {
                        // Reset local trailing flag.
                        *trailing = false;
                    }
                    // Append DW_OP_plus_uconst part.
                    let mut writer = ExpressionWriter::new();
                    writer.write_op(gimli::constants::DW_OP_plus_uconst)?;
                    writer.write_uleb128(offset as u64)?;
                    code_chunk.extend(writer.into_vec());
                    continue;
                }
                Operation::Drop { .. }
                | Operation::Pick { .. }
                | Operation::Swap { .. }
                | Operation::Rot { .. }
                | Operation::Nop { .. }
                | Operation::UnsignedConstant { .. }
                | Operation::SignedConstant { .. }
                | Operation::PlusConstant { .. }
                | Operation::And { .. }
                | Operation::Or { .. }
                | Operation::Shr { .. }
                | Operation::Shra { .. }
                | Operation::Shl { .. }
                | Operation::Plus { .. }
                | Operation::Minus { .. }
                | Operation::Piece { .. } => (),
                Operation::Bra { target } | Operation::Skip { target } => {
                    flush_code_chunk!();
                    let arc_from = (expr.0.len().into_u64() - pc.len().into_u64()) as usize;
                    let arc_to = (arc_from as i32 + target as i32) as usize;
                    jump_arcs.insert(arc_from, arc_to);
                    push!(CompiledExpressionPart::Jump {
                        target,
                        conditionally: match op {
                            Operation::Bra { .. } => true,
                            _ => false,
                        },
                    });
                    continue;
                }
                Operation::StackValue => {
                    need_deref = false;

                    // Find extra stack_value, that follow wasm-local operators,
                    // and mark such locals with special flag.
                    if let (Some(CompiledExpressionPart::Local { trailing, .. }), true) =
                        (parts.last_mut(), code_chunk.is_empty())
                    {
                        *trailing = true;
                        continue;
                    }
                }
                Operation::Deref { .. } => {
                    flush_code_chunk!();
                    push!(CompiledExpressionPart::Deref);
                    // Don't re-enter the loop here (i.e. continue), because the
                    // DW_OP_deref still needs to be kept.
                }
                _ => {
                    return Ok(None);
                }
            }
            let chunk = &buf[pos..pc.offset_from(&expr.0).into_u64() as usize];
            code_chunk.extend_from_slice(chunk);
        }
    }

    unread_bytes = 0;
    flush_code_chunk!();

    parts.push(CompiledExpressionPart::LandingPad {
        original_pos: expr.0.len().into_u64() as usize,
    });

    parts.push(CompiledExpressionPart::Code(vec![])); // so that we have enough windows below

    // collect all CompiledExpressionParts but only relevant LandingPads:
    //  - those followed by a Code chunk that is being jumped into
    //  - those which are either at the start or end of a jump arc
    let parts = parts
        .windows(2)
        .filter_map(|p| match p {
	    [CompiledExpressionPart::LandingPad { original_pos }, CompiledExpressionPart::Code(code_chunk)]
		if jump_arcs.values().any(|t| (original_pos + 1..original_pos + code_chunk.len()).contains(t))
		=> Some(p[0].clone()),
	    [CompiledExpressionPart::LandingPad { original_pos }, _]
		if jump_arcs.iter().all(|(from, to)| from != original_pos && to != original_pos)
		=> None,
	    _ => Some(p[0].clone())
	})
	.collect();
    Ok(Some(CompiledExpression {
        parts,
        need_deref,
        jump_arcs,
    }))
}

#[derive(Debug, Clone)]
struct CachedValueLabelRange {
    func_index: DefinedFuncIndex,
    start: usize,
    end: usize,
    label_location: HashMap<ValueLabel, ValueLoc>,
}

struct ValueLabelRangesBuilder<'a, 'b> {
    ranges: Vec<CachedValueLabelRange>,
    frame_info: Option<&'a FunctionFrameInfo<'b>>,
    processed_labels: HashSet<ValueLabel>,
}

impl<'a, 'b> ValueLabelRangesBuilder<'a, 'b> {
    pub fn new(
        scope: &[(u64, u64)], // wasm ranges
        addr_tr: &'a AddressTransform,
        frame_info: Option<&'a FunctionFrameInfo<'b>>,
    ) -> Self {
        let mut ranges = Vec::new();
        for (wasm_start, wasm_end) in scope {
            if let Some((func_index, tr)) = addr_tr.translate_ranges_raw(*wasm_start, *wasm_end) {
                ranges.extend(tr.into_iter().map(|(start, end)| CachedValueLabelRange {
                    func_index,
                    start,
                    end,
                    label_location: HashMap::new(),
                }));
            }
        }
        ranges.sort_unstable_by(|a, b| a.start.cmp(&b.start));
        ValueLabelRangesBuilder {
            ranges,
            frame_info,
            processed_labels: HashSet::new(),
        }
    }

    fn process_label(&mut self, label: ValueLabel) {
        if self.processed_labels.contains(&label) {
            return;
        }
        self.processed_labels.insert(label);

        let value_ranges = match self.frame_info.and_then(|fi| fi.value_ranges.get(&label)) {
            Some(value_ranges) => value_ranges,
            None => {
                return;
            }
        };

        let ranges = &mut self.ranges;
        for value_range in value_ranges {
            let range_start = value_range.start as usize;
            let range_end = value_range.end as usize;
            let loc = value_range.loc;
            if range_start == range_end {
                continue;
            }
            assert_lt!(range_start, range_end);

            // Find acceptable scope of ranges to intersect with.
            let i = match ranges.binary_search_by(|s| s.start.cmp(&range_start)) {
                Ok(i) => i,
                Err(i) => {
                    if i > 0 && range_start < ranges[i - 1].end {
                        i - 1
                    } else {
                        i
                    }
                }
            };
            let j = match ranges.binary_search_by(|s| s.start.cmp(&range_end)) {
                Ok(i) | Err(i) => i,
            };
            // Starting from the end, intersect (range_start..range_end) with
            // self.ranges array.
            for i in (i..j).rev() {
                if range_end <= ranges[i].start || ranges[i].end <= range_start {
                    continue;
                }
                if range_end < ranges[i].end {
                    // Cutting some of the range from the end.
                    let mut tail = ranges[i].clone();
                    ranges[i].end = range_end;
                    tail.start = range_end;
                    ranges.insert(i + 1, tail);
                }
                assert_le!(ranges[i].end, range_end);
                if range_start <= ranges[i].start {
                    ranges[i].label_location.insert(label, loc);
                    continue;
                }
                // Cutting some of the range from the start.
                let mut tail = ranges[i].clone();
                ranges[i].end = range_start;
                tail.start = range_start;
                tail.label_location.insert(label, loc);
                ranges.insert(i + 1, tail);
            }
        }
    }

    pub fn into_ranges(self) -> impl Iterator<Item = CachedValueLabelRange> {
        // Ranges with not-enough labels are discarded.
        let processed_labels_len = self.processed_labels.len();
        self.ranges
            .into_iter()
            .filter(move |r| r.label_location.len() == processed_labels_len)
    }
}

#[cfg(test)]
mod tests {
    use super::compile_expression;
    use super::{AddressTransform, FunctionFrameInfo, ValueLabel, ValueLabelsRanges};
    use gimli::{self, constants, Encoding, EndianSlice, Expression, RunTimeEndian};
    use wasmtime_environ::CompiledFunction;

    macro_rules! dw_op {
        (DW_OP_WASM_location) => {
            0xed
        };
        ($i:literal) => {
            $i
        };
        ($d:ident) => {
            constants::$d.0 as u8
        };
    }

    macro_rules! expression {
        ($($t:tt),*) => {
            Expression(EndianSlice::new(
                &[$(dw_op!($t)),*],
                RunTimeEndian::Little,
            ))
        }
    }

    static DWARF_ENCODING: Encoding = Encoding {
        address_size: 4,
        format: gimli::Format::Dwarf32,
        version: 4,
    };

    #[test]
    fn test_debug_parse_expressions() {
        use super::{CompiledExpression, CompiledExpressionPart};
        use std::collections::HashMap;
        use wasmtime_environ::entity::EntityRef;

        let (val1, val3, val20) = (ValueLabel::new(1), ValueLabel::new(3), ValueLabel::new(20));

        let e = expression!(DW_OP_WASM_location, 0x0, 20, DW_OP_stack_value);
        let ce = compile_expression(&e, DWARF_ENCODING, None)
            .expect("non-error")
            .expect("expression");
        assert_eq!(
            ce,
            CompiledExpression {
                parts: vec![CompiledExpressionPart::Local {
                    label: val20,
                    trailing: true
                }],
                need_deref: false,
                jump_arcs: HashMap::new()
            }
        );

        let e = expression!(
            DW_OP_WASM_location,
            0x0,
            1,
            DW_OP_plus_uconst,
            0x10,
            DW_OP_stack_value
        );
        let ce = compile_expression(&e, DWARF_ENCODING, None)
            .expect("non-error")
            .expect("expression");
        assert_eq!(
            ce,
            CompiledExpression {
                parts: vec![
                    CompiledExpressionPart::Local {
                        label: val1,
                        trailing: false
                    },
                    CompiledExpressionPart::Code(vec![35, 16, 159])
                ],
                need_deref: false,
                jump_arcs: HashMap::new()
            }
        );

        let e = expression!(DW_OP_WASM_location, 0x0, 3, DW_OP_stack_value);
        let fe = compile_expression(&e, DWARF_ENCODING, None).expect("non-error");
        let e = expression!(DW_OP_fbreg, 0x12);
        let ce = compile_expression(&e, DWARF_ENCODING, fe.as_ref())
            .expect("non-error")
            .expect("expression");
        assert_eq!(
            ce,
            CompiledExpression {
                parts: vec![
                    CompiledExpressionPart::Local {
                        label: val3,
                        trailing: false
                    },
                    CompiledExpressionPart::Code(vec![35, 18])
                ],
                need_deref: true,
                jump_arcs: HashMap::new()
            }
        );

        let e = expression!(
            DW_OP_WASM_location,
            0x0,
            1,
            DW_OP_plus_uconst,
            5,
            DW_OP_deref,
            DW_OP_stack_value
        );
        let ce = compile_expression(&e, DWARF_ENCODING, None)
            .expect("non-error")
            .expect("expression");
        assert_eq!(
            ce,
            CompiledExpression {
                parts: vec![
                    CompiledExpressionPart::Local {
                        label: val1,
                        trailing: false
                    },
                    CompiledExpressionPart::Code(vec![35, 5]),
                    CompiledExpressionPart::Deref,
                    CompiledExpressionPart::Code(vec![6, 159])
                ],
                need_deref: false,
                jump_arcs: HashMap::new()
            }
        );

        let e = expression!(
            DW_OP_lit1,
            DW_OP_dup,
            DW_OP_WASM_location,
            0x0,
            1,
            DW_OP_and,
            DW_OP_bra,
            7,
            0, // --> pointer
            DW_OP_swap,
            DW_OP_shr,
            DW_OP_skip,
            2,
            0, // --> done
            // pointer:
            DW_OP_plus,
            DW_OP_deref,
            // done:
            DW_OP_stack_value
        );
        let ce = compile_expression(&e, DWARF_ENCODING, None)
            .expect("non-error")
            .expect("expression");
        assert_eq!(
            ce,
            CompiledExpression {
                parts: vec![
                    CompiledExpressionPart::Code(vec![49, 18]),
                    CompiledExpressionPart::Local {
                        label: val1,
                        trailing: false
                    },
                    CompiledExpressionPart::Code(vec![26]),
                    CompiledExpressionPart::Jump {
                        target: 7,
                        conditionally: true
                    },
                    CompiledExpressionPart::LandingPad { original_pos: 9 }, // capture from
                    CompiledExpressionPart::Code(vec![22, 37]),
                    CompiledExpressionPart::Jump {
                        target: 2,
                        conditionally: false
                    },
                    CompiledExpressionPart::LandingPad { original_pos: 14 }, // capture from
                    CompiledExpressionPart::Code(vec![34]),
                    CompiledExpressionPart::Deref,
                    CompiledExpressionPart::LandingPad { original_pos: 16 }, // capture to
                    CompiledExpressionPart::Code(vec![159])
                ],
                need_deref: false,
                jump_arcs: {
                    let mut m = HashMap::new();
                    m.insert(9, 16);
                    m.insert(14, 16);
                    m
                }
            }
        );

        let e = expression!(
            DW_OP_lit1,
            DW_OP_dup,
            DW_OP_bra,
            2,
            0, // --> target
            DW_OP_deref,
            DW_OP_lit0,
            // target:
            DW_OP_stack_value
        );
        let ce = compile_expression(&e, DWARF_ENCODING, None)
            .expect("non-error")
            .expect("expression");
        assert_eq!(
            ce,
            CompiledExpression {
                parts: vec![
                    CompiledExpressionPart::Code(vec![49, 18]),
                    CompiledExpressionPart::Jump {
                        target: 2,
                        conditionally: true
                    },
                    CompiledExpressionPart::LandingPad { original_pos: 5 }, // capture from
                    CompiledExpressionPart::Deref,
                    CompiledExpressionPart::LandingPad { original_pos: 6 }, // capture to
                    CompiledExpressionPart::Code(vec![48, 159])
                ],
                need_deref: false,
                jump_arcs: {
                    let mut m = HashMap::new();
                    m.insert(5, 7);
                    m
                }
            }
        );

        let e = expression!(DW_OP_WASM_location, 0x0, 1, DW_OP_plus_uconst, 5);
        let ce = compile_expression(&e, DWARF_ENCODING, None)
            .expect("non-error")
            .expect("expression");
        assert_eq!(
            ce,
            CompiledExpression {
                parts: vec![
                    CompiledExpressionPart::Local {
                        label: val1,
                        trailing: false
                    },
                    CompiledExpressionPart::Code(vec![35, 5])
                ],
                need_deref: true,
                jump_arcs: HashMap::new()
            }
        );
    }

    fn create_mock_address_transform() -> AddressTransform {
        use wasmtime_environ::entity::PrimaryMap;
        use wasmtime_environ::ir::SourceLoc;
        use wasmtime_environ::WasmFileInfo;
        use wasmtime_environ::{FunctionAddressMap, InstructionAddressMap};
        let mut module_map = PrimaryMap::new();
        let code_section_offset: u32 = 100;
        module_map.push(CompiledFunction {
            address_map: FunctionAddressMap {
                instructions: vec![
                    InstructionAddressMap {
                        srcloc: SourceLoc::new(code_section_offset + 12),
                        code_offset: 5,
                        code_len: 3,
                    },
                    InstructionAddressMap {
                        srcloc: SourceLoc::new(code_section_offset + 17),
                        code_offset: 15,
                        code_len: 8,
                    },
                ],
                start_srcloc: SourceLoc::new(code_section_offset + 10),
                end_srcloc: SourceLoc::new(code_section_offset + 20),
                body_offset: 0,
                body_len: 30,
            },
            ..Default::default()
        });
        let fi = WasmFileInfo {
            code_section_offset: code_section_offset.into(),
            funcs: Vec::new(),
            imported_func_count: 0,
            path: None,
        };
        AddressTransform::new(&module_map, &fi)
    }

    fn create_mock_value_ranges() -> (ValueLabelsRanges, (ValueLabel, ValueLabel, ValueLabel)) {
        use std::collections::HashMap;
        use wasmtime_environ::entity::EntityRef;
        use wasmtime_environ::ir::{ValueLoc, ValueLocRange};
        let mut value_ranges = HashMap::new();
        let value_0 = ValueLabel::new(0);
        let value_1 = ValueLabel::new(1);
        let value_2 = ValueLabel::new(2);
        value_ranges.insert(
            value_0,
            vec![ValueLocRange {
                loc: ValueLoc::Unassigned,
                start: 0,
                end: 25,
            }],
        );
        value_ranges.insert(
            value_1,
            vec![ValueLocRange {
                loc: ValueLoc::Unassigned,
                start: 5,
                end: 30,
            }],
        );
        value_ranges.insert(
            value_2,
            vec![
                ValueLocRange {
                    loc: ValueLoc::Unassigned,
                    start: 0,
                    end: 10,
                },
                ValueLocRange {
                    loc: ValueLoc::Unassigned,
                    start: 20,
                    end: 30,
                },
            ],
        );
        (value_ranges, (value_0, value_1, value_2))
    }

    #[test]
    fn test_debug_value_range_builder() {
        use super::ValueLabelRangesBuilder;
        use wasmtime_environ::entity::EntityRef;
        use wasmtime_environ::ir::StackSlots;
        use wasmtime_environ::wasm::DefinedFuncIndex;
        use wasmtime_environ::ModuleMemoryOffset;

        let addr_tr = create_mock_address_transform();
        let stack_slots = StackSlots::new();
        let (value_ranges, value_labels) = create_mock_value_ranges();
        let fi = FunctionFrameInfo {
            memory_offset: ModuleMemoryOffset::None,
            stack_slots: &stack_slots,
            value_ranges: &value_ranges,
        };

        // No value labels, testing if entire function range coming through.
        let builder = ValueLabelRangesBuilder::new(&[(10, 20)], &addr_tr, Some(&fi));
        let ranges = builder.into_ranges().collect::<Vec<_>>();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].func_index, DefinedFuncIndex::new(0));
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].end, 30);

        // Two labels (val0@0..25 and val1@5..30), their common lifetime intersect at 5..25.
        let mut builder = ValueLabelRangesBuilder::new(&[(10, 20)], &addr_tr, Some(&fi));
        builder.process_label(value_labels.0);
        builder.process_label(value_labels.1);
        let ranges = builder.into_ranges().collect::<Vec<_>>();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 5);
        assert_eq!(ranges[0].end, 25);

        // Adds val2 with complex lifetime @0..10 and @20..30 to the previous test, and
        // also narrows range.
        let mut builder = ValueLabelRangesBuilder::new(&[(11, 17)], &addr_tr, Some(&fi));
        builder.process_label(value_labels.0);
        builder.process_label(value_labels.1);
        builder.process_label(value_labels.2);
        let ranges = builder.into_ranges().collect::<Vec<_>>();
        // Result is two ranges @5..10 and @20..23
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].start, 5);
        assert_eq!(ranges[0].end, 10);
        assert_eq!(ranges[1].start, 20);
        assert_eq!(ranges[1].end, 23);
    }
}
