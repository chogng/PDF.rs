//! Fixed, same-instance Wasm ABI for the Native browser Worker.
//!
//! These raw addresses are consumed only by the generated glue in the same
//! Worker and are never placed in a protocol frame or sent through
//! `postMessage`.

#[cfg(target_arch = "wasm32")]
use pdf_rs_browser_worker::{
    wasm_dispatch, wasm_memory_epoch, wasm_output_length, wasm_output_pointer, wasm_poll,
    wasm_prepare_input, wasm_prepare_transfer, wasm_shutdown, wasm_transfer_count,
    wasm_transfer_length, wasm_transfer_pointer,
};

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_prepare_input(length: u32) -> u32 {
    wasm_prepare_input(length)
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_prepare_transfer(index: u32, length: u32) -> u32 {
    wasm_prepare_transfer(index, length)
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_dispatch(length: u32, transfer_count: u32) -> u32 {
    wasm_dispatch(length, transfer_count)
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_poll() -> u32 {
    wasm_poll()
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_output_pointer() -> u32 {
    wasm_output_pointer()
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_output_length() -> u32 {
    wasm_output_length()
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_transfer_count() -> u32 {
    wasm_transfer_count()
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_transfer_pointer(index: u32) -> u32 {
    wasm_transfer_pointer(index)
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_transfer_length(index: u32) -> u32 {
    wasm_transfer_length(index)
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_memory_epoch() -> u32 {
    wasm_memory_epoch()
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_shutdown() -> u32 {
    wasm_shutdown()
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_abi_version() -> u32 {
    1
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_abi_hash_0() -> u32 {
    0x585f_6908
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_abi_hash_1() -> u32 {
    0x4c71_6d91
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_abi_hash_2() -> u32 {
    0xa852_d981
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_abi_hash_3() -> u32 {
    0x921a_edbd
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_abi_hash_4() -> u32 {
    0x9553_5e6c
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_abi_hash_5() -> u32 {
    0xbd1f_d501
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_abi_hash_6() -> u32 {
    0x941e_0b6a
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pdf_rs_worker_abi_hash_7() -> u32 {
    0xd621_61c9
}

fn main() {}
