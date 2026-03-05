#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::mem::size_of;

    use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
    use windows_sys::Win32::System::Memory::{
        VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READ,
        PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS, PAGE_READONLY,
        PAGE_READWRITE, PAGE_WRITECOPY,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub fn can_read(addr: i64, len: i64) -> bool {
        can_access(addr, len, false)
    }

    pub fn can_write(addr: i64, len: i64) -> bool {
        can_access(addr, len, true)
    }

    pub fn read8(addr: i64) -> Option<i64> {
        if !can_read(addr, 1) {
            return None;
        }
        let mut out = [0u8; 1];
        let mut bytes = 0usize;
        let ok = unsafe {
            // SAFETY: Windows validates process address ranges for ReadProcessMemory.
            ReadProcessMemory(
                GetCurrentProcess(),
                addr as usize as *const c_void,
                out.as_mut_ptr().cast(),
                out.len(),
                &mut bytes as *mut usize,
            )
        };
        if ok == 0 || bytes != 1 {
            None
        } else {
            Some(out[0] as i64)
        }
    }

    pub fn write8(addr: i64, value: i64) -> bool {
        if !can_write(addr, 1) {
            return false;
        }
        let in_buf = [(value & 0xff) as u8];
        let mut bytes = 0usize;
        let ok = unsafe {
            // SAFETY: Windows validates process address ranges for WriteProcessMemory.
            WriteProcessMemory(
                GetCurrentProcess(),
                addr as usize as *mut c_void,
                in_buf.as_ptr().cast(),
                in_buf.len(),
                &mut bytes as *mut usize,
            )
        };
        ok != 0 && bytes == 1
    }

    pub fn read64(addr: i64) -> Option<i64> {
        if !can_read(addr, 8) {
            return None;
        }
        let mut out = [0u8; 8];
        let mut bytes = 0usize;
        let ok = unsafe {
            // SAFETY: Windows validates process address ranges for ReadProcessMemory.
            ReadProcessMemory(
                GetCurrentProcess(),
                addr as usize as *const c_void,
                out.as_mut_ptr().cast(),
                out.len(),
                &mut bytes as *mut usize,
            )
        };
        if ok == 0 || bytes != 8 {
            None
        } else {
            Some(i64::from_ne_bytes(out))
        }
    }

    pub fn write64(addr: i64, value: i64) -> bool {
        if !can_write(addr, 8) {
            return false;
        }
        let in_buf = value.to_ne_bytes();
        let mut bytes = 0usize;
        let ok = unsafe {
            // SAFETY: Windows validates process address ranges for WriteProcessMemory.
            WriteProcessMemory(
                GetCurrentProcess(),
                addr as usize as *mut c_void,
                in_buf.as_ptr().cast(),
                in_buf.len(),
                &mut bytes as *mut usize,
            )
        };
        ok != 0 && bytes == 8
    }

    fn can_access(addr: i64, len: i64, write: bool) -> bool {
        if addr <= 0 || len <= 0 {
            return false;
        }
        let start = addr as usize;
        let span = len as usize;
        let end = match start.checked_add(span) {
            Some(v) => v,
            None => return false,
        };
        let mut cur = start;
        while cur < end {
            let mut mbi: MEMORY_BASIC_INFORMATION = unsafe {
                // SAFETY: Zero-initialized POD struct for Windows API output.
                std::mem::zeroed()
            };
            let queried = unsafe {
                // SAFETY: VirtualQuery reads metadata about an address range.
                VirtualQuery(
                    cur as *const c_void,
                    &mut mbi,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if queried == 0 {
                return false;
            }
            if mbi.State != MEM_COMMIT {
                return false;
            }
            let protect = mbi.Protect;
            if protect == 0 || (protect & PAGE_GUARD) != 0 || (protect & PAGE_NOACCESS) != 0 {
                return false;
            }
            let base = protect & 0xff;
            if write {
                if !allows_write(base) {
                    return false;
                }
            } else if !allows_read(base) {
                return false;
            }

            let region_start = mbi.BaseAddress as usize;
            let region_end = match region_start.checked_add(mbi.RegionSize) {
                Some(v) => v,
                None => return false,
            };
            if region_end <= cur {
                return false;
            }
            cur = region_end.min(end);
        }
        true
    }

    fn allows_read(base_protect: u32) -> bool {
        matches!(
            base_protect,
            PAGE_READONLY
                | PAGE_READWRITE
                | PAGE_WRITECOPY
                | PAGE_EXECUTE_READ
                | PAGE_EXECUTE_READWRITE
                | PAGE_EXECUTE_WRITECOPY
        )
    }

    fn allows_write(base_protect: u32) -> bool {
        matches!(
            base_protect,
            PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY
        )
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn can_read(addr: i64, len: i64) -> bool {
        coarse_can_access(addr, len)
    }

    pub fn can_write(addr: i64, len: i64) -> bool {
        coarse_can_access(addr, len)
    }

    pub fn read8(addr: i64) -> Option<i64> {
        if !coarse_can_access(addr, 1) {
            return None;
        }
        let ptr = addr as usize as *const u8;
        let value = unsafe {
            // SAFETY: Non-Windows fallback retains coarse validation only.
            std::ptr::read_unaligned(ptr)
        };
        Some(value as i64)
    }

    pub fn write8(addr: i64, value: i64) -> bool {
        if !coarse_can_access(addr, 1) {
            return false;
        }
        let ptr = addr as usize as *mut u8;
        unsafe {
            // SAFETY: Non-Windows fallback retains coarse validation only.
            std::ptr::write_unaligned(ptr, (value & 0xff) as u8);
        }
        true
    }

    pub fn read64(addr: i64) -> Option<i64> {
        if !coarse_can_access(addr, 8) {
            return None;
        }
        let ptr = addr as usize as *const i64;
        let value = unsafe {
            // SAFETY: Non-Windows fallback retains coarse validation only.
            std::ptr::read_unaligned(ptr)
        };
        Some(value)
    }

    pub fn write64(addr: i64, value: i64) -> bool {
        if !coarse_can_access(addr, 8) {
            return false;
        }
        let ptr = addr as usize as *mut i64;
        unsafe {
            // SAFETY: Non-Windows fallback retains coarse validation only.
            std::ptr::write_unaligned(ptr, value);
        }
        true
    }

    fn coarse_can_access(addr: i64, len: i64) -> bool {
        if addr <= 0 || len <= 0 {
            return false;
        }
        let start = addr as u64;
        let span = len as u64;
        start.checked_add(span).is_some()
    }
}

pub use imp::{can_read, can_write, read64, read8, write64, write8};
