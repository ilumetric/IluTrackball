//! SHA-256 through the cryptography provider of Windows.

use std::fmt::Write;
use std::ptr;

use windows_sys::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};

const DIGEST_BYTES: usize = 32;

/// The hash in lower case hex, the way `sha256sum` prints it. `None` if Windows
/// could not work it out.
pub fn sha256_hex(data: &[u8]) -> Option<String> {
    let mut digest = [0u8; DIGEST_BYTES];
    let status = unsafe {
        BCryptHash(
            BCRYPT_SHA256_ALG_HANDLE,
            ptr::null(),
            0,
            data.as_ptr(),
            u32::try_from(data.len()).ok()?,
            digest.as_mut_ptr(),
            DIGEST_BYTES as u32,
        )
    };
    if status != 0 {
        return None;
    }
    let mut text = String::with_capacity(DIGEST_BYTES * 2);
    for byte in digest {
        write!(text, "{byte:02x}").ok()?;
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_published_test_vectors() {
        assert_eq!(
            sha256_hex(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"").unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
