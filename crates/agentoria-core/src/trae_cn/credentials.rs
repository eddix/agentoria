use std::{
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

static TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    Missing,
    Invalid,
    Rejected,
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Missing => "Enter your Trae CN session key to open local sessions",
            Self::Invalid => "The session key must contain 64 hexadecimal characters",
            Self::Rejected => "The session key did not unlock the Trae CN database",
        })
    }
}
impl std::error::Error for KeyError {}

/// Intentionally has no Debug/Serialize implementation.
pub struct SessionKey(Zeroizing<String>);

impl SessionKey {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.len() != 64 || !input.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(KeyError::Invalid.into());
        }
        Ok(Self(Zeroizing::new(input.to_ascii_lowercase())))
    }

    pub(super) fn hex(&self) -> &str {
        &self.0
    }

    pub fn load() -> Result<Self> {
        Self::load_at(&key_path()?)
    }

    pub fn load_at(path: &Path) -> Result<Self> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(KeyError::Missing.into())
            }
            Err(error) => return Err(error).context("Could not read the saved Trae CN key file"),
        };
        let mut content = Zeroizing::new(String::new());
        file.take(4097)
            .read_to_string(&mut content)
            .context("Could not read the saved Trae CN key file")?;
        if content.len() > 4096 {
            return Err(KeyError::Invalid.into());
        }
        let mut saved: SavedKey = serde_json::from_str(&content).map_err(|_| KeyError::Invalid)?;
        let secret = Zeroizing::new(std::mem::take(&mut saved.sqlcipher_key));
        Self::parse(&secret)
    }

    pub fn save(&self) -> Result<()> {
        self.save_at(&key_path()?)
    }

    pub fn save_at(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("Key file has no parent directory")?;
        fs::create_dir_all(parent)
            .context("Could not create the Agentoria configuration directory")?;
        if fs::symlink_metadata(parent)?.file_type().is_symlink() {
            bail!("Agentoria key directory must not be a symbolic link");
        }
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            bail!("Agentoria key file must not be a symbolic link");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        let id = TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(".trae-cn-key-{}-{id}.tmp", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut created = false;
        let result = (|| -> Result<()> {
            let mut file = options
                .open(&temporary)
                .context("Could not create the private key file")?;
            created = true;
            // Serialize a borrowed field to avoid another owned copy of the secret.
            #[derive(Serialize)]
            struct BorrowedKey<'a> {
                sqlcipher_key: &'a str,
            }
            let content = Zeroizing::new(serde_json::to_vec(&BorrowedKey {
                sqlcipher_key: self.hex(),
            })?);
            file.write_all(&content)?;
            file.sync_all()?;
            fs::rename(&temporary, path).context("Could not replace the saved Trae CN key")?;
            Ok(())
        })();
        if result.is_err() && created {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

#[derive(Deserialize)]
struct SavedKey {
    sqlcipher_key: String,
}

pub fn key_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("Could not locate the home directory")?
        .join(".config/agentoria/trae-cn.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_malformed_and_oversized_saved_keys_do_not_echo_file_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("key.json");
        assert_eq!(
            SessionKey::load_at(&path)
                .err()
                .unwrap()
                .downcast_ref::<KeyError>(),
            Some(&KeyError::Missing)
        );
        for text in ["private-malformed-key".to_string(), "x".repeat(5000)] {
            fs::write(&path, &text).unwrap();
            let error = SessionKey::load_at(&path).err().unwrap();
            assert_eq!(error.downcast_ref::<KeyError>(), Some(&KeyError::Invalid));
            assert!(!format!("{error:?}").contains(&text));
        }
    }

    #[test]
    fn saves_private_key_atomically_and_rejects_invalid_input_without_echoing_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config/trae-cn.json");
        let key = SessionKey::parse(&"1a".repeat(32)).unwrap();
        key.save_at(&path).unwrap();
        assert_eq!(SessionKey::load_at(&path).unwrap().hex(), key.hex());
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        let error = SessionKey::parse("private-invalid-input").err().unwrap();
        assert!(!error.to_string().contains("private-invalid-input"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_overwrite_a_symlink_target() {
        let directory = tempfile::tempdir().unwrap();
        let outside = directory.path().join("outside");
        fs::write(&outside, "original").unwrap();
        let parent = directory.path().join("config");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("trae-cn.json");
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(SessionKey::parse(&"ab".repeat(32))
            .unwrap()
            .save_at(&path)
            .is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "original");
    }
}
