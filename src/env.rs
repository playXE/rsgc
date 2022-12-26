
fn read_float_and_factor_from_env(var: &str) -> Option<(f64, usize)> {
    let value = std::env::var(var);

    match value {
        Ok(mut value) => {
            if value.len() > 0 {
                if value.len() > 1
                    && (value.as_bytes()[value.len() - 1] == 'b' as u8
                        || value.as_bytes()[value.len() - 1] == 'B' as u8)
                {
                    value = value.as_str()[0..value.len() - 1].to_string();
                }
                let mut realvalue = value.as_str()[0..value.len() - 1].to_string();

                let at = value.len() - 1;
                let factor;
                if value.as_bytes()[at] == 'g' as u8 || value.as_bytes()[at] == 'G' as u8 {
                    factor = 1024 * 1024 * 1024;
                } else if value.as_bytes()[at] == 'm' as u8 || value.as_bytes()[at] == 'M' as u8 {
                    factor = 1024 * 1024;
                } else if value.as_bytes()[at] == 'k' as u8 || value.as_bytes()[at] == 'K' as u8 {
                    factor = 1024;
                } else {
                    realvalue = value;
                    factor = 1;
                }

                match realvalue.parse::<f64>() {
                    Ok(x) => Some((x, factor)),
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn read_uint_from_env(var: &str) -> Option<usize> {
    let (value, factor) = read_float_and_factor_from_env(var)?;

    Some(value as usize * factor)
}

pub fn read_float_from_env(var: &str) -> Option<f64> {
    read_float_and_factor_from_env(var).map(|x| x.0)
}


cfg_if::cfg_if! {
    if #[cfg(target_pointer_width="32")]
    {
        if #[cfg(linux)] {
            const ADRESSABLE_SIZE: usize = 4 * 1024 * 1024 * 1024;
        } else if #[cfg(windows)] {
            const ADRESSABLE_SIZE: usize = 2 * 1024 * 1024 * 1024;
        } else {
            const ADRESSABLE_SIZE: usize = 3 * 1024 * 1024 * 1024;
        }
    } else {
        const ADRESSABLE_SIZE: usize = 2usize.pow(63);
    }
}

fn get_total_memory_linux(_filename: &str) -> usize {
    
    #[cfg(target_os="linux")]
    {
        libc::sysconf(libc::_SC_PHYS_PAGES) as usize * libc::sysconf(libc::_SC_PAGESIZE) as usize 
    } 
    #[cfg(not(target_os="linux"))]
    {
        ADRESSABLE_SIZE
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
        let result = libc::sysctlbyname(cstr.as_ptr(), &mut buf[0] as *mut u8 as _, &mut len, null_mut(), 0);
        if result == 0 && len == size_of::<u64>() {
            let mut value = 0i64;
            std::ptr::copy_nonoverlapping(&buf[0], &mut value as *mut i64 as *mut u8, size_of::<u64>());
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
            ADRESSABLE_SIZE
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