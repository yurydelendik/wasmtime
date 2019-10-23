#[path="../examples/hello.rs"]
mod hello;
#[path="../examples/gcd.rs"]
mod gcd;
#[path="../examples/memory.rs"]
mod memory;
#[path="../examples/multi.rs"]
mod multi;

#[test]
fn test_hello_example() {
    hello::main().expect("success");
}

#[test]
fn test_gcd_example() {
    gcd::main().expect("success");
}

#[test]
fn test_memory_example() {
    memory::main().expect("success");
}

#[test]
fn test_multi_example() {
    multi::main().expect("success");
}
