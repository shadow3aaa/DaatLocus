//! Narrow WinFsp 2.x ABI declarations; functions are loaded from its installed DLL.
#![allow(non_snake_case)]
use std::ffi::c_void;
pub type Status = i32;
pub type Fs = *mut FileSystem;
pub type Context = *mut c_void;
pub type Name = *mut u16;
pub type Buffer = *mut c_void;
#[repr(C)]
pub struct FileSystem {
    pub version: u16,
    pub user_context: Context,
}
#[repr(C)]
pub struct VolumeParams {
    pub version: u16,
    pub sector_size: u16,
    pub sectors_per_allocation_unit: u16,
    pub max_component_length: u16,
    pub creation_time: u64,
    pub serial: u32,
    pub transact_timeout: u32,
    pub irp_timeout: u32,
    pub irp_capacity: u32,
    pub file_info_timeout: u32,
    pub flags: u32,
    pub prefix: [u16; 192],
    pub name: [u16; 16],
}
#[repr(C)]
pub struct VolumeInfo {
    pub total_size: u64,
    pub free_size: u64,
    pub label_length: u16,
    pub label: [u16; 32],
}
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct FileInfo {
    pub attributes: u32,
    pub reparse_tag: u32,
    pub allocation_size: u64,
    pub size: u64,
    pub creation_time: u64,
    pub access_time: u64,
    pub write_time: u64,
    pub change_time: u64,
    pub inode: u64,
    pub hard_links: u32,
    pub ea_size: u32,
}
#[repr(C)]
pub struct DirInfo {
    pub size: u16,
    pub file: FileInfo,
    pub padding: [u8; 24],
}
pub type GetReparseByName = extern "C" fn(Fs, Context, Name, u8, Buffer, *mut usize) -> Status;
#[repr(C)]
pub struct Interface {
    pub get_volume_info: Option<extern "C" fn(Fs, *mut VolumeInfo) -> Status>,
    pub set_volume_label: Option<extern "C" fn(Fs, Name, *mut VolumeInfo) -> Status>,
    pub get_security_by_name:
        Option<extern "C" fn(Fs, Name, *mut u32, Buffer, *mut usize) -> Status>,
    pub create: Option<
        extern "C" fn(Fs, Name, u32, u32, u32, Buffer, u64, *mut Context, *mut FileInfo) -> Status,
    >,
    pub open: Option<extern "C" fn(Fs, Name, u32, u32, *mut Context, *mut FileInfo) -> Status>,
    pub overwrite: Option<extern "C" fn(Fs, Context, u32, u8, u64, *mut FileInfo) -> Status>,
    pub cleanup: Option<extern "C" fn(Fs, Context, Name, u32)>,
    pub close: Option<extern "C" fn(Fs, Context)>,
    pub read: Option<extern "C" fn(Fs, Context, Buffer, u64, u32, *mut u32) -> Status>,
    pub write: Option<
        extern "C" fn(Fs, Context, Buffer, u64, u32, u8, u8, *mut u32, *mut FileInfo) -> Status,
    >,
    pub flush: Option<extern "C" fn(Fs, Context, *mut FileInfo) -> Status>,
    pub get_file_info: Option<extern "C" fn(Fs, Context, *mut FileInfo) -> Status>,
    pub set_basic_info:
        Option<extern "C" fn(Fs, Context, u32, u64, u64, u64, u64, *mut FileInfo) -> Status>,
    pub set_file_size: Option<extern "C" fn(Fs, Context, u64, u8, *mut FileInfo) -> Status>,
    pub can_delete: Option<extern "C" fn(Fs, Context, Name) -> Status>,
    pub rename: Option<extern "C" fn(Fs, Context, Name, Name, u8) -> Status>,
    pub get_security: Option<extern "C" fn(Fs, Context, Buffer, *mut usize) -> Status>,
    pub set_security: Option<extern "C" fn(Fs, Context, u32, Buffer) -> Status>,
    pub read_directory:
        Option<extern "C" fn(Fs, Context, Name, Name, Buffer, u32, *mut u32) -> Status>,
    pub resolve_reparse_points:
        Option<extern "C" fn(Fs, Name, u32, u8, Buffer, Buffer, *mut usize) -> Status>,
    pub get_reparse_point: Option<extern "C" fn(Fs, Context, Name, Buffer, *mut usize) -> Status>,
    pub set_reparse_point: Option<extern "C" fn(Fs, Context, Name, Buffer, usize) -> Status>,
    pub delete_reparse_point: Option<extern "C" fn(Fs, Context, Name, Buffer, usize) -> Status>,
    pub reserved: [usize; 41],
}
const _: () = {
    assert!(std::mem::size_of::<VolumeParams>() == 456);
    assert!(std::mem::size_of::<VolumeInfo>() == 88);
    assert!(std::mem::size_of::<FileInfo>() == 72);
    assert!(std::mem::size_of::<DirInfo>() == 104);
    assert!(std::mem::size_of::<Interface>() == 64 * std::mem::size_of::<usize>());
};
