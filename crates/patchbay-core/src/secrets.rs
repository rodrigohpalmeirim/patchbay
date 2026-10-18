//! Opt-in secret storage in the OS keychain (service name `patchbay`).
//!
//! Backed by `keyring` v4 in its `v1` mode: macOS Keychain, Windows Credential
//! Manager, or the freedesktop Secret Service (via zbus) on other Unixes. The
//! platform store is initialized lazily on first use; [`init`] forces that
//! initialization and reports whether a store is available, so the app can
//! call it once at startup (from a blocking context) to surface problems early.
//!
//! All functions are blocking (they may talk to D-Bus or a system service), so
//! async callers should run them on a blocking thread. "No entry" is never an
//! error: [`get`] returns `Ok(None)` and [`delete`] succeeds. Any other failure,
//! including "no secret store available" on headless Linux, is
//! [`Error::Keyring`]; callers should treat it as "not stored".
//!
//! CONTRACT: `session` uses these to look up / remember passwords and key
//! passphrases.

use keyring::Entry;

use crate::{Error, Result};

const SERVICE: &str = "patchbay";

/// Keychain account for a password: `password:user@host:port`.
pub fn password_account(user: &str, host: &str, port: u16) -> String {
    format!("password:{user}@{host}:{port}")
}

/// Keychain account for a key passphrase: `passphrase:<absolute key path>`.
pub fn passphrase_account(key_path: &std::path::Path) -> String {
    format!("passphrase:{}", key_path.display())
}

/// Initialize the platform credential store (idempotent). Returns an error if
/// no store is available, e.g. no Secret Service on a headless Linux box.
pub fn init() -> Result<()> {
    Entry::store_status().as_ref().map_err(map_err).copied()
}

pub fn get(account: &str) -> Result<Option<String>> {
    match entry(account)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(map_err(&e)),
    }
}

pub fn set(account: &str, secret: &str) -> Result<()> {
    entry(account)?
        .set_password(secret)
        .map_err(|e| map_err(&e))
}

pub fn delete(account: &str) -> Result<()> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(map_err(&e)),
    }
}

fn entry(account: &str) -> Result<Entry> {
    Entry::new(SERVICE, account).map_err(|e| map_err(&e))
}

fn map_err(e: &keyring::Error) -> Error {
    Error::Keyring(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_names() {
        assert_eq!(
            password_account("me", "example.com", 22),
            "password:me@example.com:22"
        );
        assert_eq!(
            passphrase_account(std::path::Path::new("/home/me/.ssh/id_ed25519")),
            "passphrase:/home/me/.ssh/id_ed25519"
        );
    }

    /// Whatever the environment, the store must either work or report an
    /// error; it must never panic.
    #[test]
    fn missing_entry_or_error() {
        match get("patchbay-test:definitely-not-stored") {
            Ok(v) => assert_eq!(v, None),
            Err(Error::Keyring(_)) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
