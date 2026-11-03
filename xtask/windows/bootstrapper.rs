//! WiX 6 BAFunctions: apply the install button's wizard visibility condition.
use std::ffi::c_void;

type Handle = *mut c_void;
type Proc = unsafe extern "system" fn(u32, Handle, Handle, Handle) -> i32;

// Prefixes of WiX's BAFunctions.h and IBootstrapperEngine.h ABI.
#[repr(C)]
struct CreateArgs {
    size: u32,
    version: u64,
    engine: Handle,
}
#[repr(C)]
struct CreateResults {
    size: u32,
    proc: Proc,
    context: Handle,
}
#[repr(C)]
struct LoadedArgs {
    size: u32,
    name: *const u16,
    id: u16,
    window: Handle,
}
#[repr(C)]
struct WindowPos {
    window: Handle,
    after: Handle,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    flags: u32,
}

#[link(name = "comctl32")]
unsafe extern "system" {
    fn SetWindowSubclass(window: Handle, proc: Subclass, id: usize, data: usize) -> i32;
    fn DefSubclassProc(window: Handle, message: u32, w: usize, l: isize) -> isize;
}
type Subclass = unsafe extern "system" fn(Handle, u32, usize, isize, usize, usize) -> isize;

unsafe extern "system" fn visibility(
    window: Handle,
    message: u32,
    w: usize,
    l: isize,
    _: usize,
    engine: usize,
) -> isize {
    unsafe {
        if message == 0x46 {
            // WM_WINDOWPOSCHANGING: WixStdBA excludes automatic visibility for
            // InstallButton, so gate its show operation on the accepted license.
            let position = &mut *(l as *mut WindowPos);
            if position.flags & 0x40 != 0 {
                let table = *(engine as *const *const usize);
                let get: unsafe extern "system" fn(Handle, *const u16, *mut i64) -> i32 =
                    std::mem::transmute(*table.add(4));
                let name: Vec<_> = "LicenseAccepted".encode_utf16().chain([0]).collect();
                let mut accepted = 0;
                if get(engine as Handle, name.as_ptr(), &mut accepted) < 0 || accepted == 0 {
                    position.flags = (position.flags & !0x40) | 0x80;
                }
            }
        }
        DefSubclassProc(window, message, w, l)
    }
}

unsafe extern "system" fn callback(message: u32, args: Handle, _: Handle, engine: Handle) -> i32 {
    unsafe {
        if message == 1029 {
            let args = &*(args as *const LoadedArgs);
            let expected: Vec<_> = "InstallButton".encode_utf16().chain([0]).collect();
            if !args.name.is_null()
                && expected
                    .iter()
                    .enumerate()
                    .all(|(i, unit)| *args.name.add(i) == *unit)
                && SetWindowSubclass(args.window, visibility, 1, engine as usize) == 0
            {
                return 0x80004005u32 as i32;
            }
        }
        0
    }
}

#[unsafe(no_mangle)]
unsafe extern "system" fn BAFunctionsCreate(
    args: *const CreateArgs,
    results: *mut CreateResults,
) -> i32 {
    unsafe {
        (*results).proc = callback;
        (*results).context = (*args).engine;
    }
    0
}
