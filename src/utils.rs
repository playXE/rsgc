pub mod math;
pub mod bytemap;
pub mod u24;
pub mod block_list;
pub mod mcontext;

cfg_if::cfg_if! {
    if #[cfg(target_pointer_width="32")]
    {
        cfg_if::cfg_if! { if #[cfg(linux)] {
                pub const ADRESSABLE_SIZE: usize = 3 * 1024 * 1024 * 1024;
            } else if #[cfg(windows)] {
                pub const ADRESSABLE_SIZE: usize = 2 * 1024 * 1024 * 1024;
            } else {
                pub const ADRESSABLE_SIZE: usize = 3 * 1024 * 1024 * 1024;
            }
        }
    } else {
        pub const ADRESSABLE_SIZE: usize = 2usize.pow(63);
    }
}

fn get_total_memory_linux(_filename: &str) -> usize {
    #[cfg(all(target_pointer_width="32", unix))]
    {
        return 3 * 1024 * 1024 * 1024
    }
    #[cfg(all(target_os = "linux", not(target_pointer_width="32")))]
    unsafe {
        libc::sysconf(libc::_SC_PHYS_PAGES) as usize * libc::sysconf(libc::_SC_PAGESIZE) as usize
    }
    #[cfg(not(target_os = "linux"))]
    {
        ADRESSABLE_SIZE
    }
}

#[cfg(target_os = "windows")]
fn get_total_memory_windows() -> usize {
    use winapi::um::sysinfoapi::GetPhysicallyInstalledSystemMemory;

    unsafe {
        let mut kilobytes = 0;
        let status = GetPhysicallyInstalledSystemMemory(&mut kilobytes);
        if status == 0 {
            ADRESSABLE_SIZE
        } else {
            kilobytes as usize * 1024
        }
    }
}

#[cfg(target_vendor = "apple")]
fn get_darwin_sysctl(name: &str) -> u64 {
    use std::ffi::CString;

    use std::mem::size_of;
    use std::ptr::null_mut;

    unsafe {
        let cstr = CString::new(name).unwrap();
        let mut len = size_of::<u64>();
        let mut buf = [0u8; 16];
        let result = libc::sysctlbyname(
            cstr.as_ptr(),
            &mut buf[0] as *mut u8 as _,
            &mut len,
            null_mut(),
            0,
        );
        if result == 0 && len == size_of::<u64>() {
            let mut value = 0i64;
            std::ptr::copy_nonoverlapping(
                &buf[0],
                &mut value as *mut i64 as *mut u8,
                size_of::<u64>(),
            );
            value as _
        } else {
            ADRESSABLE_SIZE as _
        }
    }
}

pub fn get_total_memory() -> usize {
    /*if cfg!(target_os = "linux") {
        get_total_memory_linux("/proc/meminfo")
    } else if cfg!(target_os = "windows") {
        ADRESSABLE_SIZE
    } else if cfg!(target_os = "macos") || cfg!(target_os="ios") {
        get_darwin_sysctl("hw.memsize") as _
    } else if cfg!(target_os="freebsd") {
        get_darwin_sysctl("hm.usermem") as _
    } else {
        ADRESSABLE_SIZE
    }*/

    cfg_if::cfg_if! {
        if #[cfg(target_os="linux")]
        {
            get_total_memory_linux("/proc/meminfo")
        } else if #[cfg(target_os="windows")]
        {
            get_total_memory_windows()
        } else if #[cfg(any(target_os="macos", target_os="ios", target_os="tvos", target_os="watchos"))]
        {
            get_darwin_sysctl("hw.memsize") as _
        } else if #[cfg(target_os="freebsd")]
        {
            get_darwin_sysctl("hm.usermem") as _
        } else {
            ADRESSABLE_SIZE
        }
    }
}

pub struct FormattedSize {
    pub size: f64,
}

impl std::fmt::Display for FormattedSize {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let ksize = (self.size as f64) / 1024f64;

        if ksize < 1f64 {
            return write!(f, "{}B", self.size);
        }

        let msize = ksize / 1024f64;

        if msize < 1f64 {
            return write!(f, "{:.1}K", ksize);
        }

        let gsize = msize / 1024f64;

        if gsize < 8f64 {
            write!(f, "{:.1}M", msize)
        } else {
            write!(f, "{:.1}G", gsize)
        }
    }
}

impl std::fmt::Debug for FormattedSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self)
    }
}

pub fn formatted_size(size: usize) -> FormattedSize {
    FormattedSize { size: size as f64 }
}

pub fn formatted_sizef(size: f64) -> FormattedSize {
    FormattedSize { size: size as f64 }
}