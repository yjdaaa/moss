//! NV15 Windows user-scoped DPAPI envelope for HA bearer credentials.
//! Never put plaintext credentials into SQLite/diagnostics/backups. The
//! dependency-free non-Windows implementation fails closed (not plaintext).
const PREFIX: &str = "dpapi-user-v1:";
const ENTROPY: &[u8] = b"moss-rust.homeassistant.token.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeSecretError {
    Unsupported,
    Invalid,
    CryptoFailure,
}

pub(crate) fn is_sealed(value: &str) -> bool {
    value.starts_with(PREFIX)
}

#[cfg(windows)]
#[repr(C)]
struct DataBlob {
    len: u32,
    data: *mut u8,
}

#[cfg(windows)]
#[link(name = "Crypt32")]
unsafe extern "system" {
    fn CryptProtectData(
        input: *const DataBlob,
        description: *const u16,
        entropy: *const DataBlob,
        reserved: *const std::ffi::c_void,
        prompt: *const std::ffi::c_void,
        flags: u32,
        output: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        input: *const DataBlob,
        description: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *const std::ffi::c_void,
        prompt: *const std::ffi::c_void,
        flags: u32,
        output: *mut DataBlob,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "Kernel32")]
unsafe extern "system" {
    fn LocalFree(pointer: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

#[cfg(windows)]
fn crypt(input: &[u8], decrypt: bool) -> Result<Vec<u8>, HomeSecretError> {
    use std::ptr;
    if input.is_empty() || input.len() > 64 * 1024 {
        return Err(HomeSecretError::Invalid);
    }
    let mut source = DataBlob { len: input.len() as u32, data: input.as_ptr().cast_mut() };
    let mut entropy = DataBlob { len: ENTROPY.len() as u32, data: ENTROPY.as_ptr().cast_mut() };
    let mut output = DataBlob { len: 0, data: ptr::null_mut() };
    // Safety: Windows DPAPI takes borrowed buffers and allocates the result
    // with LocalAlloc. We always release via LocalFree, even on malformed data.
    let success = unsafe {
        if decrypt {
            CryptUnprotectData(&mut source, ptr::null_mut(), &mut entropy,
                ptr::null(), ptr::null(), 0x1 /* UI_FORBIDDEN */, &mut output)
        } else {
            CryptProtectData(&mut source, ptr::null(), &mut entropy,
                ptr::null(), ptr::null(), 0x1 /* UI_FORBIDDEN */, &mut output)
        }
    };
    if success == 0 || output.data.is_null() || output.len == 0 || output.len > 64 * 1024 {
        if !output.data.is_null() {
            unsafe { LocalFree(output.data.cast()); }
        }
        return Err(HomeSecretError::CryptoFailure);
    }
    let result = unsafe { std::slice::from_raw_parts(output.data, output.len as usize).to_vec() };
    if decrypt {
        // Avoid leaving another plaintext copy in a freed LocalAlloc buffer.
        unsafe {
            for index in 0..output.len as usize {
                std::ptr::write_volatile(output.data.add(index), 0);
            }
        }
    }
    unsafe { LocalFree(output.data.cast()); }
    Ok(result)
}

#[cfg(windows)]
pub(crate) fn seal(token: &str) -> Result<String, HomeSecretError> {
    Ok(format!("{PREFIX}{}", hex::encode(crypt(token.as_bytes(), false)?)))
}

#[cfg(windows)]
pub(crate) fn open(ciphertext: &str) -> Result<String, HomeSecretError> {
    let encoded = ciphertext.strip_prefix(PREFIX).ok_or(HomeSecretError::Invalid)?;
    let bytes = hex::decode(encoded).map_err(|_| HomeSecretError::Invalid)?;
    String::from_utf8(crypt(&bytes, true)?).map_err(|_| HomeSecretError::Invalid)
}

#[cfg(not(windows))]
pub(crate) fn seal(_token: &str) -> Result<String, HomeSecretError> {
    Err(HomeSecretError::Unsupported)
}

#[cfg(not(windows))]
pub(crate) fn open(_ciphertext: &str) -> Result<String, HomeSecretError> {
    Err(HomeSecretError::Unsupported)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn dpapi_user_scope_round_trip_with_corruption_rejected() {
        let original = "nv15-synthetic-not-real-token-abcdefghijklmnopqrstuvwxyz";
        let encrypted = seal(original).unwrap();
        assert!(is_sealed(&encrypted));
        assert!(!encrypted.contains(original));
        assert_eq!(open(&encrypted).unwrap(), original);
        let mut tampered = encrypted.clone();
        tampered.replace_range(PREFIX.len()..PREFIX.len()+2, "ff");
        assert!(open(&tampered).is_err());
        assert!(open(original).is_err());
    }
}
