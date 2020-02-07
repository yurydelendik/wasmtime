use anyhow::{anyhow, bail, Context as _, Result};
use faerie::Artifact;
use std::fs::File;
use target_lexicon::Triple;
use wasmtime_debug::{emit_debugsections, read_debuginfo};
use wasmtime_environ::{
    entity::EntityRef, settings, settings::Configurable, wasm::DefinedMemoryIndex,
    wasm::MemoryIndex, Compiler, Cranelift, ModuleEnvironment, ModuleMemoryOffset, ModuleVmctxInfo,
    Tunables, VMOffsets,
};
use wasmtime_jit::native;
use wasmtime_obj::emit_module;

pub fn compile_cranelift(wasm: &[u8], target: Option<Triple>, output: &str) -> Result<()> {
    let isa_builder = match target {
        Some(target) => native::lookup(target.clone())?,
        None => native::builder(),
    };
    let mut flag_builder = settings::builder();

    // There are two possible traps for division, and this way
    // we get the proper one if code traps.
    flag_builder.enable("avoid_div_traps").unwrap();

    // if optimize {
    //     flag_builder.set("opt_level", "speed").unwrap();
    // }

    let isa = isa_builder.finish(settings::Flags::new(flag_builder));

    let mut obj = Artifact::new(isa.triple().clone(), output.to_string());

    // TODO: Expose the tunables as command-line flags.
    let tunables = Tunables::default();

    let (
        module,
        module_translation,
        lazy_function_body_inputs,
        lazy_data_initializers,
        target_config,
    ) = {
        let environ = ModuleEnvironment::new(isa.frontend_config(), tunables);

        let translation = environ
            .translate(wasm)
            .context("failed to translate module")?;

        (
            translation.module,
            translation.module_translation.unwrap(),
            translation.function_body_inputs,
            translation.data_initializers,
            translation.target_config,
        )
    };

    // TODO: use the traps information
    let (compilation, relocations, address_transform, value_ranges, stack_slots, _traps) =
        Cranelift::compile_module(
            &module,
            &module_translation,
            lazy_function_body_inputs,
            &*isa,
            /* debug_info = */ true,
        )
        .context("failed to compile module")?;

    if compilation.is_empty() {
        bail!("no functions were found/compiled");
    }

    let module_vmctx_info = {
        let ofs = VMOffsets::new(target_config.pointer_bytes(), &module);
        ModuleVmctxInfo {
            memory_offset: if ofs.num_imported_memories > 0 {
                ModuleMemoryOffset::Imported(ofs.vmctx_vmmemory_import(MemoryIndex::new(0)))
            } else if ofs.num_defined_memories > 0 {
                ModuleMemoryOffset::Defined(
                    ofs.vmctx_vmmemory_definition_base(DefinedMemoryIndex::new(0)),
                )
            } else {
                ModuleMemoryOffset::None
            },
            stack_slots,
        }
    };

    emit_module(
        &mut obj,
        &module,
        &compilation,
        &relocations,
        &lazy_data_initializers,
        &target_config,
    )
    .map_err(|e| anyhow!(e))
    .context("failed to emit module")?;

    let debug_data = read_debuginfo(wasm);
    emit_debugsections(
        &mut obj,
        &module_vmctx_info,
        target_config,
        &debug_data,
        &address_transform,
        &value_ranges,
    )
    .context("failed to emit debug sections")?;

    // FIXME: Make the format a parameter.
    let file = File::create(output).context("failed to create object file")?;
    obj.write(file).context("failed to write object file")?;

    Ok(())
}
