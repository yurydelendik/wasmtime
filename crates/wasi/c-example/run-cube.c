#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <inttypes.h>

#include "wasm.h"

#define own

WASM_API_EXTERN own wasm_module_t* wasmtime_wasi_module_new(wasm_store_t*);

void print_mutability(wasm_mutability_t mut) {
  switch (mut) {
    case WASM_VAR: printf("var"); break;
    case WASM_CONST: printf("const"); break;
  }
}

void print_limits(const wasm_limits_t* limits) {
  printf("%ud", limits->min);
  if (limits->max < wasm_limits_max_default) printf(" %ud", limits->max);
}

void print_valtype(const wasm_valtype_t* type) {
  switch (wasm_valtype_kind(type)) {
    case WASM_I32: printf("i32"); break;
    case WASM_I64: printf("i64"); break;
    case WASM_F32: printf("f32"); break;
    case WASM_F64: printf("f64"); break;
    case WASM_ANYREF: printf("anyref"); break;
    case WASM_FUNCREF: printf("funcref"); break;
  }
}

void print_valtypes(const wasm_valtype_vec_t* types) {
  bool first = true;
  for (size_t i = 0; i < types->size; ++i) {
    if (first) {
      first = false;
    } else {
      printf(" ");
    }
    print_valtype(types->data[i]);
  }
}

void print_externtype(const wasm_externtype_t* type) {
  switch (wasm_externtype_kind(type)) {
    case WASM_EXTERN_FUNC: {
      const wasm_functype_t* functype =
        wasm_externtype_as_functype_const(type);
      printf("func ");
      print_valtypes(wasm_functype_params(functype));
      printf(" -> ");
      print_valtypes(wasm_functype_results(functype));
    } break;
    case WASM_EXTERN_GLOBAL: {
      const wasm_globaltype_t* globaltype =
        wasm_externtype_as_globaltype_const(type);
      printf("global ");
      print_mutability(wasm_globaltype_mutability(globaltype));
      printf(" ");
      print_valtype(wasm_globaltype_content(globaltype));
    } break;
    case WASM_EXTERN_TABLE: {
      const wasm_tabletype_t* tabletype =
        wasm_externtype_as_tabletype_const(type);
      printf("table ");
      print_limits(wasm_tabletype_limits(tabletype));
      printf(" ");
      print_valtype(wasm_tabletype_element(tabletype));
    } break;
    case WASM_EXTERN_MEMORY: {
      const wasm_memorytype_t* memorytype =
        wasm_externtype_as_memorytype_const(type);
      printf("memory ");
      print_limits(wasm_memorytype_limits(memorytype));
    } break;
  }
}

void print_name(const wasm_name_t* name) {
  printf("\"%.*s\"", (int)name->size, name->data);
}

bool is_name_same(const wasm_name_t* name, const wasm_name_t* other) {
  if (name->size != other->size) return false;
  return memcmp(name->data, other->data, other->size) == 0;
}

int main(int argc, const char* argv[]) {
  // Initialize.
  printf("Initializing...\n");
  wasm_engine_t* engine = wasm_engine_new();
  wasm_store_t* store = wasm_store_new(engine);

  // Load binary.
  printf("Loading binary...\n");
  FILE* file = fopen("cube.wasm", "r");
  if (!file) {
    printf("> Error loading module!\n");
    return 1;
  }
  fseek(file, 0L, SEEK_END);
  size_t file_size = ftell(file);
  fseek(file, 0L, SEEK_SET);
  wasm_byte_vec_t binary;
  wasm_byte_vec_new_uninitialized(&binary, file_size);
  if (fread(binary.data, file_size, 1, file) != 1) {
    printf("> Error loading module!\n");
    return 1;
  }
  fclose(file);

  // Compile.
  printf("Compiling module...\n");
  own wasm_module_t* module = wasm_module_new(store, &binary);
  if (!module) {
    printf("> Error compiling module!\n");
    return 1;
  }

  wasm_byte_vec_delete(&binary);

  // Instantiate WASI.
  printf("WASI module...\n");
  own wasm_module_t* wasi_module = wasmtime_wasi_module_new(store);
  if (!wasi_module) {
    printf("> Error getting WASI module!\n");
    return 1;
  }

  
  printf("Instantiating WASI module...\n");
  own wasm_instance_t* wasi_instance =
    wasm_instance_new(store, wasi_module, NULL, NULL);
  if (!wasi_instance) {
    printf("> Error instantiating WASI module!\n");
    return 1;
  }

  printf("Extracting WASI export...\n");
  own wasm_extern_vec_t wasi_exports;
  wasm_instance_exports(wasi_instance, &wasi_exports);
  if (wasi_exports.size == 0) {
    printf("> Error accessing WASI exports!\n");
    return 1;
  }

  printf("Matching WASI imports...\n");
  own wasm_importtype_vec_t import_types;
  wasm_module_imports(module, &import_types);
  own wasm_exporttype_vec_t export_types;
  wasm_module_exports(wasi_module, &export_types);

  const wasm_extern_t** imports = malloc(sizeof(const wasm_extern_t*) *  import_types.size);
  for (size_t i = 0; i < import_types.size; ++i) {
    const wasm_name_t* import_name = wasm_importtype_name(import_types.data[i]);
    imports[i] = NULL;
    for (size_t j = 0; j < export_types.size; ++j) {
      if (!is_name_same(import_name, wasm_exporttype_name(export_types.data[j]))) continue;
      imports[i] = wasi_exports.data[j];
      break;
    }
    if (!imports[i]) {
      printf("> Import ");
      print_name(import_name);
      printf(" not found\n");
      return 1;
    }
  }

  wasm_importtype_vec_delete(&import_types);
  wasm_exporttype_vec_delete(&export_types);

  const wasm_func_t* run_func = wasm_extern_as_func(wasi_exports.data[0]);
  if (run_func == NULL) {
    printf("> Error accessing WASI export!\n");
    return 1;
  }

  // Instantiate.
  printf("Instantiating module...\n");
  own wasm_instance_t* instance =
    wasm_instance_new(store, module, imports, NULL);
  if (!instance) {
    printf("> Error instantiating module!\n");
    return 1;
  }

  wasm_extern_vec_delete(&wasi_exports);

  // All done.

  wasm_module_delete(module);
  wasm_instance_delete(instance);


  // Shut down.
  printf("Shutting down...\n");
  wasm_store_delete(store);
  wasm_engine_delete(engine);

  // All done.
  printf("Done.\n");
  return 0;
}
