//! Object file generation.

use super::compiler::build_trampoline;
use cranelift_codegen::ir::LibCall;
use cranelift_frontend::FunctionBuilderContext;
use object::{self, write::Object};
use std::collections::HashMap;
use wasmtime_debug::DwarfSection;
use wasmtime_environ::entity::{EntityRef, PrimaryMap};
use wasmtime_environ::isa::{unwind::UnwindInfo, TargetIsa};
use wasmtime_environ::wasm::{FuncIndex, SignatureIndex};
use wasmtime_environ::{Module, Relocation, RelocationTarget, Relocations};

fn to_object_relocations<'a>(
    it: impl Iterator<Item = &'a Relocation> + 'a,
    off: u64,
    funcs: &'a PrimaryMap<FuncIndex, object::write::SymbolId>,
    libcalls: &'a HashMap<LibCall, object::write::SymbolId>,
) -> impl Iterator<Item = object::write::Relocation> + 'a {
    use cranelift_codegen::binemit::Reloc;
    use object::{RelocationEncoding, RelocationKind};

    it.filter_map(move |r| {
        let (kind, encoding, size) = match r.reloc {
            Reloc::Abs4 => (RelocationKind::Absolute, RelocationEncoding::Generic, 32),
            Reloc::Abs8 => (RelocationKind::Absolute, RelocationEncoding::Generic, 64),
            Reloc::X86PCRel4 => (RelocationKind::Relative, RelocationEncoding::Generic, 32),
            Reloc::X86CallPCRel4 => (RelocationKind::Relative, RelocationEncoding::X86Branch, 32),
            // TODO: Get Cranelift to tell us when we can use
            // R_X86_64_GOTPCRELX/R_X86_64_REX_GOTPCRELX.
            Reloc::X86CallPLTRel4 => (
                RelocationKind::PltRelative,
                RelocationEncoding::X86Branch,
                32,
            ),
            Reloc::X86GOTPCRel4 => (RelocationKind::GotRelative, RelocationEncoding::Generic, 32),
            Reloc::ElfX86_64TlsGd => (
                RelocationKind::Elf(object::elf::R_X86_64_TLSGD),
                RelocationEncoding::Generic,
                32,
            ),
            Reloc::X86PCRelRodata4 => {
                return None;
            }
            // FIXME
            _ => unimplemented!(),
        };
        let symbol = match r.reloc_target {
            RelocationTarget::UserFunc(index) => funcs[index],
            RelocationTarget::LibCall(call) => libcalls[&call],
            // FIXME
            _ => unimplemented!(),
        };
        Some(object::write::Relocation {
            offset: off + r.offset as u64,
            size,
            kind,
            encoding,
            symbol,
            addend: r.addend,
        })
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObjectUnwindInfo {
    Func(FuncIndex, UnwindInfo),
    Trampoline(SignatureIndex, UnwindInfo),
}

pub(crate) fn build_object(
    isa: &dyn TargetIsa,
    compilation: &wasmtime_environ::Compilation,
    relocations: &Relocations,
    module: &Module,
    dwarf_sections: &[DwarfSection],
) -> Result<(Object, Vec<ObjectUnwindInfo>), anyhow::Error> {
    const CODE_SECTION_ALIGNMENT: u64 = 0x1000;
    let mut obj = Object::new(
        object::BinaryFormat::Elf,
        object::Architecture::X86_64,
        object::Endianness::Little,
    );
    let section_id = obj.add_section(
        obj.segment_name(object::write::StandardSegment::Text)
            .to_vec(),
        ".text".as_bytes().to_vec(),
        object::SectionKind::Text,
    );

    let mut unwind_info = Vec::new();

    let mut func_symbols = PrimaryMap::with_capacity(compilation.len());
    for index in 0..module.local.num_imported_funcs {
        let symbol_id = obj.add_symbol(object::write::Symbol {
            name: format!("_wasm_function_{}", index).as_bytes().to_vec(),
            value: 0,
            size: 0,
            kind: object::SymbolKind::Text,
            scope: object::SymbolScope::Linkage,
            weak: true,
            section: object::write::SymbolSection::Undefined,
            flags: object::SymbolFlags::None,
        });
        func_symbols.push(symbol_id);
    }
    for (index, func) in compilation.into_iter().enumerate() {
        let off = obj.append_section_data(section_id, &func.body, 1);
        let symbol_id = obj.add_symbol(object::write::Symbol {
            name: format!("_wasm_function_{}", module.local.num_imported_funcs + index)
                .as_bytes()
                .to_vec(),
            value: off,
            size: func.body.len() as u64,
            kind: object::SymbolKind::Text,
            scope: object::SymbolScope::Compilation,
            weak: false,
            section: object::write::SymbolSection::Section(section_id),
            flags: object::SymbolFlags::None,
        });
        func_symbols.push(symbol_id);
        if let Some(UnwindInfo::WindowsX64(info)) = &func.unwind_info {
            let unwind_size = info.emit_size();
            let mut unwind_info = vec![0; unwind_size];
            info.emit(&mut unwind_info);
            let _off = obj.append_section_data(section_id, &unwind_info, 4);
        }
        if let Some(info) = &func.unwind_info {
            unwind_info.push(ObjectUnwindInfo::Func(
                FuncIndex::new(module.local.num_imported_funcs + index),
                info.clone(),
            ))
        }
    }

    let mut libcalls = HashMap::new();
    macro_rules! add_libcall_symbol {
        ($i:ident, $name:ident) => {{
            let symbol_id = obj.add_symbol(object::write::Symbol {
                name: stringify!($name).as_bytes().to_vec(),
                value: 0,
                size: 0,
                kind: object::SymbolKind::Text,
                scope: object::SymbolScope::Linkage,
                weak: true,
                section: object::write::SymbolSection::Undefined,
                flags: object::SymbolFlags::None,
            });
            libcalls.insert(LibCall::$i, symbol_id);
        }};
    }
    add_libcall_symbol!(UdivI64, wasmtime_i64_udiv);
    add_libcall_symbol!(UdivI64, wasmtime_i64_udiv);
    add_libcall_symbol!(SdivI64, wasmtime_i64_sdiv);
    add_libcall_symbol!(UremI64, wasmtime_i64_urem);
    add_libcall_symbol!(SremI64, wasmtime_i64_srem);
    add_libcall_symbol!(IshlI64, wasmtime_i64_ishl);
    add_libcall_symbol!(UshrI64, wasmtime_i64_ushr);
    add_libcall_symbol!(SshrI64, wasmtime_i64_sshr);
    add_libcall_symbol!(CeilF32, wasmtime_f32_ceil);
    add_libcall_symbol!(FloorF32, wasmtime_f32_floor);
    add_libcall_symbol!(TruncF32, wasmtime_f32_trunc);
    add_libcall_symbol!(NearestF32, wasmtime_f32_nearest);
    add_libcall_symbol!(CeilF64, wasmtime_f64_ceil);
    add_libcall_symbol!(FloorF64, wasmtime_f64_floor);
    add_libcall_symbol!(TruncF64, wasmtime_f64_trunc);
    add_libcall_symbol!(NearestF64, wasmtime_f64_nearest);

    let mut trampoline_relocs = HashMap::new();
    let mut cx = FunctionBuilderContext::new();
    for (i, (_, native_sig)) in module.local.signatures.iter() {
        let (func, relocs) =
            build_trampoline(isa, &mut cx, native_sig, std::mem::size_of::<u128>())?;
        let off = obj.append_section_data(section_id, &func.body, 1);
        let symbol_id = obj.add_symbol(object::write::Symbol {
            name: format!("_trampoline_{}", i.index()).as_bytes().to_vec(),
            value: off,
            size: func.body.len() as u64,
            kind: object::SymbolKind::Text,
            scope: object::SymbolScope::Compilation,
            weak: false,
            section: object::write::SymbolSection::Section(section_id),
            flags: object::SymbolFlags::None,
        });
        trampoline_relocs.insert(symbol_id, relocs);
        if let Some(UnwindInfo::WindowsX64(info)) = &func.unwind_info {
            let unwind_size = info.emit_size();
            let mut unwind_info = vec![0; unwind_size];
            info.emit(&mut unwind_info);
            let _off = obj.append_section_data(section_id, &unwind_info, 4);
        }
        if let Some(info) = &func.unwind_info {
            unwind_info.push(ObjectUnwindInfo::Trampoline(i, info.clone()))
        }
    }
    obj.append_section_data(section_id, &[], CODE_SECTION_ALIGNMENT);

    let (debug_bodies, debug_relocs) = dwarf_sections
        .into_iter()
        .map(|s| ((s.name.as_str(), &s.body), (s.name.as_str(), &s.relocs)))
        .unzip::<_, _, Vec<_>, Vec<_>>();

    let mut dwarf_sections_ids = HashMap::new();
    for (name, body) in debug_bodies {
        let segment = obj
            .segment_name(object::write::StandardSegment::Debug)
            .to_vec();
        let section_id = obj.add_section(
            segment,
            name.as_bytes().to_vec(),
            object::SectionKind::Debug,
        );
        dwarf_sections_ids.insert(name.to_string(), section_id);
        obj.append_section_data(section_id, &body, 1);
    }

    for (index, relocs) in relocations.into_iter() {
        let func_index = module.local.func_index(index);
        let (_, off) = obj
            .symbol_section_and_offset(func_symbols[func_index])
            .unwrap();
        for r in to_object_relocations(relocs.iter(), off, &func_symbols, &libcalls) {
            obj.add_relocation(section_id, r)?;
        }
    }
    for (symbol, relocs) in trampoline_relocs {
        let (_, off) = obj.symbol_section_and_offset(symbol).unwrap();
        for r in to_object_relocations(relocs.iter(), off, &func_symbols, &libcalls) {
            obj.add_relocation(section_id, r)?;
        }
    }

    for (name, relocs) in debug_relocs {
        let section_id = *dwarf_sections_ids.get(name).unwrap();
        for reloc in relocs {
            let target_symbol = if reloc.target.starts_with("_wasm_function") {
                // Debug information bases index as defined function, we need
                // module function index.
                let index = reloc.target["_wasm_function_".len()..].parse().unwrap();
                func_symbols[FuncIndex::new(index)]
            } else {
                obj.section_symbol(*dwarf_sections_ids.get(&reloc.target).unwrap())
            };
            obj.add_relocation(
                section_id,
                object::write::Relocation {
                    offset: u64::from(reloc.offset),
                    size: reloc.size << 3,
                    kind: object::RelocationKind::Absolute,
                    encoding: object::RelocationEncoding::Generic,
                    symbol: target_symbol,
                    addend: i64::from(reloc.addend),
                },
            )?;
        }
    }

    let mut file = ::std::fs::File::create(::std::path::Path::new("test.o")).expect("file");
    ::std::io::Write::write_all(&mut file, &obj.write().expect("obj")).expect("write");

    Ok((obj, unwind_info))
}
