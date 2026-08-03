use std::path::Path;

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;

use crate::error::{AppError, AppResult};

use super::AppData;

#[cfg(windows)]
const SECRET_PREFIX: &str = "dpapi:v1:";
#[cfg(unix)]
const SECRET_PREFIX: &str = "aesgcm:v1:";

pub fn protect_secrets(data: &AppData, data_path: &Path) -> AppResult<AppData> {
    let mut protected = data.clone();
    visit_secret_values(&mut protected, |value| protect_value(value, data_path))?;
    Ok(protected)
}

/// Decrypt protected values and report whether plaintext legacy values were found.
pub fn unprotect_secrets(data: &mut AppData, data_path: &Path) -> AppResult<bool> {
    let mut found_plaintext = false;
    visit_secret_values(data, |value| {
        if value.is_empty() {
            return Ok(value.to_string());
        }
        if value.starts_with(SECRET_PREFIX) {
            unprotect_value(value, data_path)
        } else {
            found_plaintext = true;
            Ok(value.to_string())
        }
    })?;
    Ok(found_plaintext)
}

fn visit_secret_values(
    data: &mut AppData,
    mut transform: impl FnMut(&str) -> AppResult<String>,
) -> AppResult<()> {
    for value in data.shared_secrets.values_mut() {
        *value = transform(value)?;
    }
    for secrets in data.workspace_secrets.values_mut() {
        for value in secrets.values_mut() {
            *value = transform(value)?;
        }
    }
    for secrets in data.app_secrets.values_mut() {
        for value in secrets.values_mut() {
            *value = transform(value)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn protect_value(value: &str, _data_path: &Path) -> AppResult<String> {
    use windows::core::w;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    if value.is_empty() {
        return Ok(String::new());
    }
    let bytes = value.as_bytes();
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes
            .len()
            .try_into()
            .map_err(|_| AppError::Message("secret is too large to encrypt".into()))?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            w!("Coding Tools MCP secret"),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|error| AppError::Message(format!("DPAPI encryption failed: {error}")))?;
        let encrypted = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let encoded = STANDARD_NO_PAD.encode(encrypted);
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(format!("{SECRET_PREFIX}{encoded}"))
    }
}

#[cfg(windows)]
fn unprotect_value(value: &str, _data_path: &Path) -> AppResult<String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut encrypted = STANDARD_NO_PAD
        .decode(value.trim_start_matches(SECRET_PREFIX))
        .map_err(|error| AppError::Message(format!("invalid encrypted secret: {error}")))?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: encrypted
            .len()
            .try_into()
            .map_err(|_| AppError::Message("encrypted secret is too large".into()))?,
        pbData: encrypted.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|error| {
            AppError::Message(format!(
                "DPAPI decryption failed; this data belongs to another Windows user or is damaged: {error}"
            ))
        })?;
        let decrypted = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let text = String::from_utf8(decrypted.to_vec()).map_err(|error| {
            AppError::Message(format!("decrypted secret is not UTF-8: {error}"))
        });
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        text
    }
}

#[cfg(unix)]
fn protect_value(value: &str, data_path: &Path) -> AppResult<String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};
    use rand_core::{OsRng, RngCore};

    if value.is_empty() {
        return Ok(String::new());
    }
    let key = load_or_create_portable_key(data_path)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|error| {
        AppError::Message(format!("secret cipher initialization failed: {error}"))
    })?;
    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), value.as_bytes())
        .map_err(|error| AppError::Message(format!("secret encryption failed: {error}")))?;
    Ok(format!(
        "{SECRET_PREFIX}{}:{}",
        STANDARD_NO_PAD.encode(nonce),
        STANDARD_NO_PAD.encode(encrypted)
    ))
}

#[cfg(unix)]
fn unprotect_value(value: &str, data_path: &Path) -> AppResult<String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};

    let payload = value.trim_start_matches(SECRET_PREFIX);
    let (nonce, encrypted) = payload
        .split_once(':')
        .ok_or_else(|| AppError::Message("invalid encrypted secret envelope".into()))?;
    let nonce = STANDARD_NO_PAD
        .decode(nonce)
        .map_err(|error| AppError::Message(format!("invalid secret nonce: {error}")))?;
    if nonce.len() != 12 {
        return Err(AppError::Message("invalid secret nonce length".into()));
    }
    let encrypted = STANDARD_NO_PAD
        .decode(encrypted)
        .map_err(|error| AppError::Message(format!("invalid encrypted secret: {error}")))?;
    let key = load_or_create_portable_key(data_path)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|error| {
        AppError::Message(format!("secret cipher initialization failed: {error}"))
    })?;
    let decrypted = cipher
        .decrypt(Nonce::from_slice(&nonce), encrypted.as_ref())
        .map_err(|_| {
            AppError::Message("secret decryption failed; key is missing or data is damaged".into())
        })?;
    String::from_utf8(decrypted)
        .map_err(|error| AppError::Message(format!("decrypted secret is not UTF-8: {error}")))
}

#[cfg(unix)]
fn load_or_create_portable_key(data_path: &Path) -> AppResult<[u8; 32]> {
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;

    use rand_core::{OsRng, RngCore};

    let parent = data_path
        .parent()
        .ok_or_else(|| AppError::Message("data path has no parent directory".into()))?;
    let key_path = parent.join(".profiles.key");
    if key_path.exists() {
        let mut key = [0_u8; 32];
        std::fs::File::open(&key_path)?.read_exact(&mut key)?;
        return Ok(key);
    }

    let mut key = [0_u8; 32];
    OsRng.fill_bytes(&mut key);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_path)
    {
        Ok(mut file) => {
            file.write_all(&key)?;
            file.sync_all()?;
            Ok(key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut existing = [0_u8; 32];
            std::fs::File::open(&key_path)?.read_exact(&mut existing)?;
            Ok(existing)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_maps_are_encrypted_at_rest_and_round_trip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("profiles.json");
        let mut data = AppData::default();
        data.shared_secrets
            .insert("bearer_token".into(), "plain-secret".into());

        let mut protected = protect_secrets(&data, &path).expect("protect");
        let disk_value = protected
            .shared_secrets
            .get("bearer_token")
            .expect("protected value");
        assert_ne!(disk_value, "plain-secret");
        assert!(disk_value.starts_with(SECRET_PREFIX));
        assert!(!unprotect_secrets(&mut protected, &path).expect("unprotect"));
        assert_eq!(
            protected
                .shared_secrets
                .get("bearer_token")
                .map(String::as_str),
            Some("plain-secret")
        );
    }

    #[test]
    fn plaintext_values_are_reported_for_migration() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("profiles.json");
        let mut data = AppData::default();
        data.shared_secrets
            .insert("bearer_token".into(), "legacy-secret".into());

        assert!(unprotect_secrets(&mut data, &path).expect("read legacy"));
        assert_eq!(data.shared_secrets["bearer_token"], "legacy-secret");
    }
}
