//! NV15 OS user-scoped credential sealing for HA bearer credentials:
//! - Windows: current-user DPAPI (`dpapi-user-v1:<hex>`);
//! - macOS: current-user Login Keychain device-only master key + Encrypt-then-HMAC-SHA256
//!   authenticated envelope (`keychain-user-v1:<hex>`);
//! - Other platforms: dependency-free fail-closed (`Unsupported`, never plaintext).
const DPAPI_PREFIX: &str = "dpapi-user-v1:";
const KEYCHAIN_PREFIX: &str = "keychain-user-v1:";
const ENTROPY: &[u8] = b"moss-rust.homeassistant.token.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeSecretError {
    #[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
    Unsupported,
    Invalid,
    CryptoFailure,
}

pub(crate) fn is_sealed(value: &str) -> bool {
    value.starts_with(DPAPI_PREFIX) || value.starts_with(KEYCHAIN_PREFIX)
}

#[cfg(any(target_os = "macos", test))]
mod envelope {
    use super::{ENTROPY, HomeSecretError};
    use sha2::{Digest, Sha256};

    pub(super) const MASTER_KEY_LEN: usize = 32;
    pub(super) const NONCE_LEN: usize = 16;
    const TAG_LEN: usize = 32;
    const MAX_PLAINTEXT_LEN: usize = 64 * 1024;

    pub(super) fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
        let mut key_block = [0u8; 64];
        if key.len() > 64 {
            let hashed = Sha256::digest(key);
            key_block[..32].copy_from_slice(&hashed);
        } else {
            key_block[..key.len()].copy_from_slice(key);
        }
        let mut ipad = [0x36u8; 64];
        let mut opad = [0x5cu8; 64];
        for i in 0..64 {
            ipad[i] ^= key_block[i];
            opad[i] ^= key_block[i];
        }
        let mut inner = Sha256::new();
        inner.update(ipad);
        for part in parts {
            inner.update(part);
        }
        let inner_digest = inner.finalize();
        let mut outer = Sha256::new();
        outer.update(opad);
        outer.update(inner_digest);
        let final_digest = outer.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&final_digest);
        for b in &mut key_block {
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        out
    }

    fn xor_keystream(enc_key: &[u8; 32], input: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; input.len()];
        for (block_idx, (dst_chunk, src_chunk)) in
            out.chunks_mut(32).zip(input.chunks(32)).enumerate()
        {
            let counter = (block_idx as u64).to_le_bytes();
            let stream = hmac_sha256(enc_key, &[&counter]);
            for (d, (s, k)) in dst_chunk
                .iter_mut()
                .zip(src_chunk.iter().zip(stream.iter()))
            {
                *d = *s ^ *k;
            }
        }
        out
    }

    fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }

    pub(super) fn seal_with_key(
        master: &[u8; MASTER_KEY_LEN],
        nonce: &[u8; NONCE_LEN],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, HomeSecretError> {
        if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_LEN {
            return Err(HomeSecretError::Invalid);
        }
        let mut enc_key = hmac_sha256(master, &[b"moss.ha.enc.v1", ENTROPY, nonce]);
        let mut mac_key = hmac_sha256(master, &[b"moss.ha.mac.v1", ENTROPY, nonce]);
        let ciphertext = xor_keystream(&enc_key, plaintext);
        let len_le = (ciphertext.len() as u64).to_le_bytes();
        let tag = hmac_sha256(&mac_key, &[ENTROPY, nonce, &len_le, &ciphertext]);
        for b in enc_key.iter_mut().chain(mac_key.iter_mut()) {
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        let mut sealed = Vec::with_capacity(NONCE_LEN + ciphertext.len() + TAG_LEN);
        sealed.extend_from_slice(nonce);
        sealed.extend_from_slice(&ciphertext);
        sealed.extend_from_slice(&tag);
        Ok(sealed)
    }

    pub(super) fn open_with_key(
        master: &[u8; MASTER_KEY_LEN],
        sealed: &[u8],
    ) -> Result<Vec<u8>, HomeSecretError> {
        if sealed.len() <= NONCE_LEN + TAG_LEN
            || sealed.len() > NONCE_LEN + MAX_PLAINTEXT_LEN + TAG_LEN
        {
            return Err(HomeSecretError::Invalid);
        }
        let nonce: &[u8; NONCE_LEN] = sealed[..NONCE_LEN]
            .try_into()
            .map_err(|_| HomeSecretError::Invalid)?;
        let ciphertext = &sealed[NONCE_LEN..sealed.len() - TAG_LEN];
        let tag = &sealed[sealed.len() - TAG_LEN..];

        let mut mac_key = hmac_sha256(master, &[b"moss.ha.mac.v1", ENTROPY, nonce]);
        let len_le = (ciphertext.len() as u64).to_le_bytes();
        let expected_tag = hmac_sha256(&mac_key, &[ENTROPY, nonce, &len_le, ciphertext]);
        for b in &mut mac_key {
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        if !constant_time_eq(&expected_tag, tag) {
            return Err(HomeSecretError::CryptoFailure);
        }
        let mut enc_key = hmac_sha256(master, &[b"moss.ha.enc.v1", ENTROPY, nonce]);
        let plaintext = xor_keystream(&enc_key, ciphertext);
        for b in &mut enc_key {
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        Ok(plaintext)
    }
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
    let source = DataBlob {
        len: input.len() as u32,
        data: input.as_ptr().cast_mut(),
    };
    let entropy = DataBlob {
        len: ENTROPY.len() as u32,
        data: ENTROPY.as_ptr().cast_mut(),
    };
    let mut output = DataBlob {
        len: 0,
        data: ptr::null_mut(),
    };
    // Safety: Windows DPAPI takes borrowed buffers and allocates the result
    // with LocalAlloc. We always release via LocalFree, even on malformed data.
    let success = unsafe {
        if decrypt {
            CryptUnprotectData(
                &source,
                ptr::null_mut(),
                &entropy,
                ptr::null(),
                ptr::null(),
                0x1, /* UI_FORBIDDEN */
                &mut output,
            )
        } else {
            CryptProtectData(
                &source,
                ptr::null(),
                &entropy,
                ptr::null(),
                ptr::null(),
                0x1, /* UI_FORBIDDEN */
                &mut output,
            )
        }
    };
    if success == 0 || output.data.is_null() || output.len == 0 || output.len > 64 * 1024 {
        if !output.data.is_null() {
            unsafe {
                LocalFree(output.data.cast());
            }
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
    unsafe {
        LocalFree(output.data.cast());
    }
    Ok(result)
}

#[cfg(windows)]
pub(crate) fn seal(token: &str) -> Result<String, HomeSecretError> {
    Ok(format!(
        "{DPAPI_PREFIX}{}",
        hex::encode(crypt(token.as_bytes(), false)?)
    ))
}

#[cfg(windows)]
pub(crate) fn open(ciphertext: &str) -> Result<String, HomeSecretError> {
    let encoded = ciphertext
        .strip_prefix(DPAPI_PREFIX)
        .ok_or(HomeSecretError::Invalid)?;
    let bytes = hex::decode(encoded).map_err(|_| HomeSecretError::Invalid)?;
    String::from_utf8(crypt(&bytes, true)?).map_err(|_| HomeSecretError::Invalid)
}

#[cfg(target_os = "macos")]
mod macos_keychain {
    // 测试构建改用进程内主密钥后，钥匙串读写函数只在正式构建里使用
    #![cfg_attr(any(test, feature = "test-memory-secret-store"), allow(dead_code))]
    use super::envelope::{self, MASTER_KEY_LEN, NONCE_LEN};
    use super::{HomeSecretError, KEYCHAIN_PREFIX};
    use std::ffi::c_void;
    use std::ptr;

    type CFTypeRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFMutableDictionaryRef = *mut c_void;
    type CFStringRef = *const c_void;
    type CFDataRef = *const c_void;
    type CFAllocatorRef = *const c_void;
    type CFIndex = isize;
    type OSStatus = i32;

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const ERR_SEC_SUCCESS: OSStatus = 0;
    const ERR_SEC_ITEM_NOT_FOUND: OSStatus = -25300;
    const ERR_SEC_DUPLICATE_ITEM: OSStatus = -25299;

    const SERVICE: &[u8] = b"com.moss.laser.homeassistant.master-v1";
    const ACCOUNT: &[u8] = b"default";

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFBooleanTrue: CFTypeRef;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;

        fn CFDictionaryCreateMutable(
            allocator: CFAllocatorRef,
            capacity: CFIndex,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFMutableDictionaryRef;
        fn CFDictionarySetValue(
            dict: CFMutableDictionaryRef,
            key: *const c_void,
            value: *const c_void,
        );
        fn CFStringCreateWithBytes(
            alloc: CFAllocatorRef,
            bytes: *const u8,
            num_bytes: CFIndex,
            encoding: u32,
            is_external_representation: u8,
        ) -> CFStringRef;
        fn CFDataCreate(allocator: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
        fn CFDataGetLength(the_data: CFDataRef) -> CFIndex;
        fn CFDataGetBytePtr(the_data: CFDataRef) -> *const u8;
        fn CFRelease(cf: CFTypeRef);
    }

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        static kSecClass: CFStringRef;
        static kSecClassGenericPassword: CFStringRef;
        static kSecAttrService: CFStringRef;
        static kSecAttrAccount: CFStringRef;
        static kSecAttrAccessible: CFStringRef;
        static kSecAttrAccessibleWhenUnlockedThisDeviceOnly: CFStringRef;
        static kSecValueData: CFStringRef;
        static kSecReturnData: CFStringRef;
        static kSecMatchLimit: CFStringRef;
        static kSecMatchLimitOne: CFStringRef;
        static kSecRandomDefault: *const c_void;

        fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
        fn SecItemAdd(attributes: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
        fn SecRandomCopyBytes(rnd: *const c_void, count: usize, bytes: *mut u8) -> i32;
    }

    struct CfGuard(CFTypeRef);
    impl Drop for CfGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn cf_string(bytes: &[u8]) -> Result<CfGuard, HomeSecretError> {
        let raw = unsafe {
            CFStringCreateWithBytes(
                ptr::null(),
                bytes.as_ptr(),
                bytes.len() as CFIndex,
                K_CF_STRING_ENCODING_UTF8,
                0,
            )
        };
        if raw.is_null() {
            Err(HomeSecretError::CryptoFailure)
        } else {
            Ok(CfGuard(raw))
        }
    }

    fn cf_data(bytes: &[u8]) -> Result<CfGuard, HomeSecretError> {
        let raw = unsafe { CFDataCreate(ptr::null(), bytes.as_ptr(), bytes.len() as CFIndex) };
        if raw.is_null() {
            Err(HomeSecretError::CryptoFailure)
        } else {
            Ok(CfGuard(raw))
        }
    }

    fn sec_random(dest: &mut [u8]) -> Result<(), HomeSecretError> {
        let status =
            unsafe { SecRandomCopyBytes(kSecRandomDefault, dest.len(), dest.as_mut_ptr()) };
        if status == 0 {
            Ok(())
        } else {
            Err(HomeSecretError::CryptoFailure)
        }
    }

    fn query_master_key() -> Result<Option<[u8; MASTER_KEY_LEN]>, HomeSecretError> {
        let service = cf_string(SERVICE)?;
        let account = cf_string(ACCOUNT)?;
        let dict = unsafe {
            CFDictionaryCreateMutable(
                ptr::null(),
                5,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            )
        };
        if dict.is_null() {
            return Err(HomeSecretError::CryptoFailure);
        }
        let _dict_guard = CfGuard(dict.cast());
        unsafe {
            CFDictionarySetValue(dict, kSecClass, kSecClassGenericPassword);
            CFDictionarySetValue(dict, kSecAttrService, service.0);
            CFDictionarySetValue(dict, kSecAttrAccount, account.0);
            CFDictionarySetValue(dict, kSecReturnData, kCFBooleanTrue);
            CFDictionarySetValue(dict, kSecMatchLimit, kSecMatchLimitOne);
        }
        let mut out: CFTypeRef = ptr::null();
        let status = unsafe { SecItemCopyMatching(dict.cast(), &mut out) };
        if status == ERR_SEC_ITEM_NOT_FOUND {
            return Ok(None);
        }
        if status != ERR_SEC_SUCCESS || out.is_null() {
            return Err(HomeSecretError::Unsupported);
        }
        let _out_guard = CfGuard(out);
        let len = unsafe { CFDataGetLength(out) };
        let ptr = unsafe { CFDataGetBytePtr(out) };
        if len != MASTER_KEY_LEN as CFIndex || ptr.is_null() {
            return Err(HomeSecretError::CryptoFailure);
        }
        let mut key = [0u8; MASTER_KEY_LEN];
        unsafe { ptr::copy_nonoverlapping(ptr, key.as_mut_ptr(), MASTER_KEY_LEN) };
        Ok(Some(key))
    }

    /// 正式构建：主密钥存放在当前用户登录钥匙串（device-only）。
    #[cfg(not(any(test, feature = "test-memory-secret-store")))]
    fn load_or_create_master_key(
        create_if_missing: bool,
    ) -> Result<[u8; MASTER_KEY_LEN], HomeSecretError> {
        load_or_create_keychain_master_key(create_if_missing)
    }

    /// 测试构建：主密钥只存在于本进程内存，绝不读写用户真实登录钥匙串。
    /// 否则测试会把生产同名条目写进钥匙串，且每次重新编译的测试二进制都会触发授权弹窗导致 `cargo test` 卡死。
    /// 仅通过 dev-dependency 的 `test-memory-secret-store` feature 或本 crate 单元测试启用，正式打包不含 dev-dependency。
    #[cfg(any(test, feature = "test-memory-secret-store"))]
    fn load_or_create_master_key(
        create_if_missing: bool,
    ) -> Result<[u8; MASTER_KEY_LEN], HomeSecretError> {
        static TEST_MASTER_KEY: std::sync::Mutex<Option<[u8; MASTER_KEY_LEN]>> =
            std::sync::Mutex::new(None);
        let mut slot = TEST_MASTER_KEY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = *slot {
            return Ok(existing);
        }
        if !create_if_missing {
            return Err(HomeSecretError::CryptoFailure);
        }
        let mut generated = [0u8; MASTER_KEY_LEN];
        sec_random(&mut generated)?;
        *slot = Some(generated);
        Ok(generated)
    }

    fn load_or_create_keychain_master_key(
        create_if_missing: bool,
    ) -> Result<[u8; MASTER_KEY_LEN], HomeSecretError> {
        if let Some(existing) = query_master_key()? {
            return Ok(existing);
        }
        if !create_if_missing {
            return Err(HomeSecretError::CryptoFailure);
        }
        let mut generated = [0u8; MASTER_KEY_LEN];
        sec_random(&mut generated)?;
        let service = cf_string(SERVICE)?;
        let account = cf_string(ACCOUNT)?;
        let value = cf_data(&generated)?;
        let dict = unsafe {
            CFDictionaryCreateMutable(
                ptr::null(),
                5,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            )
        };
        if dict.is_null() {
            for b in &mut generated {
                unsafe { ptr::write_volatile(b, 0) };
            }
            return Err(HomeSecretError::CryptoFailure);
        }
        let _dict_guard = CfGuard(dict.cast());
        unsafe {
            CFDictionarySetValue(dict, kSecClass, kSecClassGenericPassword);
            CFDictionarySetValue(dict, kSecAttrService, service.0);
            CFDictionarySetValue(dict, kSecAttrAccount, account.0);
            CFDictionarySetValue(
                dict,
                kSecAttrAccessible,
                kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            );
            CFDictionarySetValue(dict, kSecValueData, value.0);
        }
        let status = unsafe { SecItemAdd(dict.cast(), ptr::null_mut()) };
        if status == ERR_SEC_SUCCESS {
            return Ok(generated);
        }
        for b in &mut generated {
            unsafe { ptr::write_volatile(b, 0) };
        }
        if status == ERR_SEC_DUPLICATE_ITEM {
            return query_master_key()?.ok_or(HomeSecretError::CryptoFailure);
        }
        Err(HomeSecretError::Unsupported)
    }

    pub(crate) fn seal(token: &str) -> Result<String, HomeSecretError> {
        if token.is_empty() || token.len() > 64 * 1024 {
            return Err(HomeSecretError::Invalid);
        }
        let mut master = load_or_create_master_key(true)?;
        let mut nonce = [0u8; NONCE_LEN];
        let nonce_res = sec_random(&mut nonce);
        if let Err(err) = nonce_res {
            for b in &mut master {
                unsafe { ptr::write_volatile(b, 0) };
            }
            return Err(err);
        }
        let res = envelope::seal_with_key(&master, &nonce, token.as_bytes());
        for b in &mut master {
            unsafe { ptr::write_volatile(b, 0) };
        }
        Ok(format!("{KEYCHAIN_PREFIX}{}", hex::encode(res?)))
    }

    pub(crate) fn open(ciphertext: &str) -> Result<String, HomeSecretError> {
        let encoded = ciphertext
            .strip_prefix(KEYCHAIN_PREFIX)
            .ok_or(HomeSecretError::Invalid)?;
        let bytes = hex::decode(encoded).map_err(|_| HomeSecretError::Invalid)?;
        let mut master = load_or_create_master_key(false)?;
        let res = envelope::open_with_key(&master, &bytes);
        for b in &mut master {
            unsafe { ptr::write_volatile(b, 0) };
        }
        String::from_utf8(res?).map_err(|_| HomeSecretError::Invalid)
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn seal(token: &str) -> Result<String, HomeSecretError> {
    macos_keychain::seal(token)
}

#[cfg(target_os = "macos")]
pub(crate) fn open(ciphertext: &str) -> Result<String, HomeSecretError> {
    macos_keychain::open(ciphertext)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) fn seal(_token: &str) -> Result<String, HomeSecretError> {
    Err(HomeSecretError::Unsupported)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) fn open(_ciphertext: &str) -> Result<String, HomeSecretError> {
    Err(HomeSecretError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_both_windows_and_macos_sealed_prefixes() {
        assert!(is_sealed("dpapi-user-v1:deadbeef"));
        assert!(is_sealed("keychain-user-v1:deadbeef"));
        assert!(!is_sealed("plaintext-token"));
    }

    #[test]
    fn rfc4231_hmac_sha256_vector_and_envelope_tamper_rejection() {
        let key = [0x0bu8; 20];
        let mac = envelope::hmac_sha256(&key, &[b"Hi There"]);
        assert_eq!(
            hex::encode(mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );

        let master = [0x42u8; envelope::MASTER_KEY_LEN];
        let other_master = [0x99u8; envelope::MASTER_KEY_LEN];
        let nonce = [0x11u8; envelope::NONCE_LEN];
        let original = b"nv15-synthetic-macos-envelope-token-0123456789";

        let sealed = envelope::seal_with_key(&master, &nonce, original).unwrap();
        assert!(!sealed.windows(original.len()).any(|w| w == original));
        assert_eq!(envelope::open_with_key(&master, &sealed).unwrap(), original);
        assert_eq!(
            envelope::open_with_key(&other_master, &sealed),
            Err(HomeSecretError::CryptoFailure)
        );

        for pos in [0, envelope::NONCE_LEN, sealed.len() - 1] {
            let mut tampered = sealed.clone();
            tampered[pos] ^= 0x01;
            assert_eq!(
                envelope::open_with_key(&master, &tampered),
                Err(HomeSecretError::CryptoFailure)
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_user_scope_round_trip_with_corruption_rejected() {
        let original = "nv15-synthetic-not-real-token-abcdefghijklmnopqrstuvwxyz";
        let sealed = seal(original).unwrap();
        assert!(is_sealed(&sealed));
        assert!(!sealed.contains(original));
        assert_eq!(open(&sealed).unwrap(), original);
        assert!(open(original).is_err());
        assert!(open("dpapi-user-v1:not-hex").is_err());
        assert!(open("keychain-user-v1:deadbeef").is_err());
        let mut damaged = sealed.into_bytes();
        let last = damaged.len() - 1;
        damaged[last] = if damaged[last] == b'0' { b'1' } else { b'0' };
        assert!(open(&String::from_utf8(damaged).unwrap()).is_err());
    }
}
