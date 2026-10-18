//! User authentication for one hop.
//!
//! Order: agent keys, identity files, keyboard-interactive, password, each
//! only while the server still lists the method as acceptable. After a
//! partial success (multi-factor setups) the sequence is run again with
//! whatever methods the server asks for next.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::{AgentAuthError, MethodKind, MethodSet};

use super::agent;
use super::handler::ClientHandler;
use super::prompt::Prompter;
use crate::model::{ConnectTarget, KbdPrompt, Prompt, PromptResponse};
use crate::{Error, Result, secrets};

const MAX_TRIES: usize = 3;
/// Bound on multi-factor rounds (each needs a partial success to continue).
const MAX_PASSES: usize = 4;

pub(crate) struct Authenticator<'a> {
    handle: &'a mut Handle<ClientHandler>,
    target: &'a ConnectTarget,
    prompter: &'a Prompter,
    timeout: Duration,
    /// Methods the server still accepts.
    methods: MethodSet,
    /// Public keys already offered, so identity files the agent has already
    /// tried are skipped (and their passphrases never asked for).
    offered: Vec<PublicKey>,
    rsa_hash: Option<Option<HashAlg>>,
    /// Partial successes so far.
    partials: usize,
    /// A password (typed or stored) was rejected earlier, by either the
    /// keyboard-interactive or the password method.
    password_retry: bool,
    /// The stored password was rejected (and deleted) already.
    stored_password_rejected: bool,
}

/// Outcome of one authentication request.
enum Step {
    Done,
    /// Accepted, but the server wants more (multi-factor).
    Partial,
    Rejected,
}

impl<'a> Authenticator<'a> {
    pub(crate) fn new(
        handle: &'a mut Handle<ClientHandler>,
        target: &'a ConnectTarget,
        prompter: &'a Prompter,
        timeout: Duration,
    ) -> Self {
        Self {
            handle,
            target,
            prompter,
            timeout,
            methods: MethodSet::empty(),
            offered: Vec::new(),
            rsa_hash: None,
            partials: 0,
            password_retry: false,
            stored_password_rejected: false,
        }
    }

    pub(crate) async fn run(mut self) -> Result<()> {
        let user = self.target.user.clone();
        let res = net(
            self.timeout,
            self.target,
            self.handle.authenticate_none(user),
        )
        .await?;
        if let Step::Done = self.record(res) {
            return Ok(());
        }
        tracing::debug!(host = %self.target.label, methods = ?&*self.methods, "server auth methods");

        for _ in 0..MAX_PASSES {
            let partials = self.partials;
            if self.allows(MethodKind::PublicKey) && self.agent_keys().await? {
                return Ok(());
            }
            if self.allows(MethodKind::PublicKey) && self.identity_files().await? {
                return Ok(());
            }
            if self.allows(MethodKind::KeyboardInteractive) && self.keyboard_interactive().await? {
                return Ok(());
            }
            if self.allows(MethodKind::Password) && self.password().await? {
                return Ok(());
            }
            if self.partials == partials {
                break;
            }
        }
        Err(Error::AuthFailed {
            user: self.target.user.clone(),
            host: self.target.label.clone(),
        })
    }

    /// The server still accepts `method` and the client config permits it.
    fn allows(&self, method: MethodKind) -> bool {
        let wanted = &self.target.auth_methods;
        let permitted = match method {
            MethodKind::PublicKey => wanted.publickey,
            MethodKind::KeyboardInteractive => wanted.keyboard_interactive,
            MethodKind::Password => wanted.password,
            _ => true,
        };
        permitted && self.methods.contains(&method)
    }

    fn record(&mut self, res: AuthResult) -> Step {
        match res {
            AuthResult::Success => Step::Done,
            AuthResult::Failure {
                remaining_methods,
                partial_success,
            } => {
                self.methods = remaining_methods;
                if partial_success {
                    tracing::debug!(methods = ?&*self.methods, "partial success");
                    self.partials += 1;
                    Step::Partial
                } else {
                    Step::Rejected
                }
            }
        }
    }

    async fn rsa_hash(&mut self) -> Option<HashAlg> {
        if self.rsa_hash.is_none() {
            let hash = self
                .handle
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .flatten();
            self.rsa_hash = Some(hash);
        }
        self.rsa_hash.flatten()
    }

    async fn hash_for(&mut self, key: &PublicKey) -> Option<HashAlg> {
        if key.algorithm().is_rsa() {
            self.rsa_hash().await
        } else {
            None
        }
    }

    async fn agent_keys(&mut self) -> Result<bool> {
        let Some(mut agent) = agent::connect(&self.target.agent).await else {
            return Ok(false);
        };
        let identities = match agent.request_identities().await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::debug!("listing agent identities failed: {e}");
                return Ok(false);
            }
        };
        let allowed: Option<Vec<PublicKey>> = self.target.identities_only.then(|| {
            self.target
                .identity_files
                .iter()
                .filter_map(|p| read_public_key(p))
                .collect()
        });

        let mut keys: Vec<PublicKey> = Vec::new();
        for identity in &identities {
            let key = match identity {
                AgentIdentity::PublicKey { key, .. } => key.clone(),
                AgentIdentity::Certificate { .. } => continue,
            };
            let permitted = allowed
                .as_ref()
                .is_none_or(|allowed| allowed.iter().any(|a| a.key_data() == key.key_data()));
            if permitted && !keys.iter().any(|k| k.key_data() == key.key_data()) {
                keys.push(key);
            }
        }

        for key in keys {
            if !self.allows(MethodKind::PublicKey) {
                break;
            }
            if self.offered.iter().any(|k| k.key_data() == key.key_data()) {
                continue;
            }
            let hash = self.hash_for(&key).await;
            self.offered.push(key.clone());
            tracing::debug!(key = %key.fingerprint(HashAlg::Sha256), "trying agent key");
            let user = self.target.user.clone();
            let fut = self
                .handle
                .authenticate_publickey_with(user, key, hash, &mut agent);
            let res = match tokio::time::timeout(self.timeout, fut).await {
                Ok(Ok(res)) => res,
                Ok(Err(AgentAuthError::Key(e))) => {
                    tracing::debug!("agent signing failed: {e}");
                    continue;
                }
                Ok(Err(AgentAuthError::Send(_))) => return Err(russh::Error::SendError.into()),
                Err(_) => return Err(timeout_error(self.target)),
            };
            match self.record(res) {
                Step::Done => return Ok(true),
                Step::Partial => return Ok(false),
                Step::Rejected => {}
            }
        }
        Ok(false)
    }

    async fn identity_files(&mut self) -> Result<bool> {
        let target = self.target;
        for path in &target.identity_files {
            if !self.allows(MethodKind::PublicKey) {
                break;
            }
            if !path.is_file() {
                tracing::debug!(path = %path.display(), "identity file not found");
                continue;
            }
            if let Some(public) = read_public_key(path)
                && self
                    .offered
                    .iter()
                    .any(|k| k.key_data() == public.key_data())
            {
                continue;
            }
            let Some(key) = self.load_key(path).await? else {
                continue;
            };
            let public = key.public_key().clone();
            let hash = self.hash_for(&public).await;
            self.offered.push(public);
            tracing::debug!(path = %path.display(), "trying identity file");
            let user = self.target.user.clone();
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
            let res = net(
                self.timeout,
                self.target,
                self.handle.authenticate_publickey(user, key),
            )
            .await?;
            match self.record(res) {
                Step::Done => return Ok(true),
                Step::Partial => return Ok(false),
                Step::Rejected => {}
            }
        }
        Ok(false)
    }

    /// Load and, if necessary, decrypt a private key. `None` means "skip".
    async fn load_key(&self, path: &Path) -> Result<Option<PrivateKey>> {
        let text = match tokio::fs::read_to_string(path).await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(path = %path.display(), "cannot read identity file: {e}");
                return Ok(None);
            }
        };
        match russh::keys::decode_secret_key(&text, None) {
            Ok(key) => return Ok(Some(key)),
            Err(russh::keys::Error::KeyIsEncrypted) => {}
            Err(e) => {
                tracing::warn!(path = %path.display(), "unusable identity file: {e}");
                return Ok(None);
            }
        }

        let account = secrets::passphrase_account(path);
        let mut retry = false;
        if let Some(stored) = secret_get(&account).await {
            match decrypt(&text, &stored).await {
                Some(key) => return Ok(Some(key)),
                None => {
                    tracing::info!(path = %path.display(), "stored passphrase is wrong; forgetting it");
                    secret_delete(&account).await;
                    retry = true;
                }
            }
        }

        for _ in 0..MAX_TRIES {
            let prompt = Prompt::KeyPassphrase {
                key_path: path.to_path_buf(),
                retry,
            };
            let (value, remember) = match self.prompter.ask(prompt).await {
                Ok(PromptResponse::Secret { value, remember }) if !value.is_empty() => {
                    (value, remember)
                }
                Ok(_) | Err(Error::Cancelled) => return Ok(None),
                Err(e) => return Err(e),
            };
            if let Some(key) = decrypt(&text, &value).await {
                if remember {
                    secret_set(&account, &value).await;
                }
                return Ok(Some(key));
            }
            retry = true;
        }
        tracing::info!(path = %path.display(), "giving up on encrypted key after {MAX_TRIES} attempts");
        Ok(None)
    }

    async fn keyboard_interactive(&mut self) -> Result<bool> {
        // Many servers implement plain password login as a keyboard-interactive
        // round with a single hidden "Password:" prompt (PAM). Those rounds go
        // through the password prompt and keychain like the `password` method.
        let account =
            secrets::password_account(&self.target.user, &self.target.hostname, self.target.port);
        let mut stored = self.stored_password(&account).await;
        let tries = MAX_TRIES + usize::from(stored.is_some());

        for _ in 0..tries {
            if !self.allows(MethodKind::KeyboardInteractive) {
                break;
            }
            let user = self.target.user.clone();
            let mut response = net(
                self.timeout,
                self.target,
                self.handle
                    .authenticate_keyboard_interactive_start(user, None),
            )
            .await?;
            let mut answered = false;
            // Password given in this attempt, and whether the last round was it.
            let mut password: Option<KbdPassword> = None;
            let mut last_was_password = false;
            loop {
                match response {
                    KeyboardInteractiveAuthResponse::Success => {
                        if let Some(KbdPassword::Typed {
                            value,
                            remember: true,
                        }) = password
                        {
                            secret_set(&account, &value).await;
                        }
                        return Ok(true);
                    }
                    KeyboardInteractiveAuthResponse::Failure {
                        remaining_methods,
                        partial_success,
                    } => {
                        let step = self.record(AuthResult::Failure {
                            remaining_methods,
                            partial_success,
                        });
                        if let Step::Partial = step {
                            if let Some(KbdPassword::Typed {
                                value,
                                remember: true,
                            }) = password
                            {
                                secret_set(&account, &value).await;
                            }
                            return Ok(false);
                        }
                        if last_was_password {
                            self.password_retry = true;
                            if let Some(KbdPassword::Stored) = password {
                                self.forget_stored_password(&account).await;
                            }
                        }
                        break;
                    }
                    KeyboardInteractiveAuthResponse::InfoRequest {
                        name,
                        instructions,
                        prompts,
                    } => {
                        // Zero-prompt rounds (OpenSSH sends one to finish a PAM
                        // conversation) don't change what the last real question was.
                        if !prompts.is_empty() {
                            last_was_password = is_password_round(&prompts);
                        }
                        let answers = if prompts.is_empty() {
                            Vec::new()
                        } else if last_was_password {
                            answered = true;
                            let value = match stored.take() {
                                Some(value) => {
                                    password = Some(KbdPassword::Stored);
                                    value
                                }
                                None => {
                                    let (value, remember) = self.ask_password().await?;
                                    password = Some(KbdPassword::Typed {
                                        value: value.clone(),
                                        remember,
                                    });
                                    value
                                }
                            };
                            vec![value]
                        } else {
                            answered = true;
                            self.ask_kbd(name, instructions, prompts).await?
                        };
                        response = net(
                            self.timeout,
                            self.target,
                            self.handle
                                .authenticate_keyboard_interactive_respond(answers),
                        )
                        .await?;
                    }
                }
            }
            // A server that fails us without ever asking anything will not
            // do better on a second try.
            if !answered {
                break;
            }
        }
        Ok(false)
    }

    async fn ask_kbd(
        &self,
        name: String,
        instructions: String,
        prompts: Vec<russh::client::Prompt>,
    ) -> Result<Vec<String>> {
        let count = prompts.len();
        let prompt = Prompt::KeyboardInteractive {
            user: self.target.user.clone(),
            host: self.target.label.clone(),
            name,
            instructions,
            prompts: prompts
                .into_iter()
                .map(|p| KbdPrompt {
                    text: p.prompt,
                    echo: p.echo,
                })
                .collect(),
        };
        match self.prompter.ask(prompt).await? {
            PromptResponse::Answers { mut answers } => {
                answers.resize(count, String::new());
                Ok(answers)
            }
            _ => Err(Error::Cancelled),
        }
    }

    async fn password(&mut self) -> Result<bool> {
        let account =
            secrets::password_account(&self.target.user, &self.target.hostname, self.target.port);

        if let Some(stored) = self.stored_password(&account).await {
            match self.try_password(stored).await? {
                Step::Done => return Ok(true),
                Step::Partial => return Ok(false),
                Step::Rejected => {
                    self.password_retry = true;
                    self.forget_stored_password(&account).await;
                }
            }
        }

        for _ in 0..MAX_TRIES {
            if !self.allows(MethodKind::Password) {
                break;
            }
            let (value, remember) = self.ask_password().await?;
            let step = self.try_password(value.clone()).await?;
            if remember && !matches!(step, Step::Rejected) {
                secret_set(&account, &value).await;
            }
            match step {
                Step::Done => return Ok(true),
                Step::Partial => return Ok(false),
                Step::Rejected => self.password_retry = true,
            }
        }
        Ok(false)
    }

    /// The keychain password, unless it was already found to be wrong.
    async fn stored_password(&self, account: &str) -> Option<String> {
        if self.stored_password_rejected {
            return None;
        }
        secret_get(account).await
    }

    async fn forget_stored_password(&mut self, account: &str) {
        tracing::info!(host = %self.target.label, "stored password was rejected; forgetting it");
        self.stored_password_rejected = true;
        secret_delete(account).await;
    }

    async fn ask_password(&self) -> Result<(String, bool)> {
        let prompt = Prompt::Password {
            user: self.target.user.clone(),
            host: self.target.label.clone(),
            retry: self.password_retry,
        };
        match self.prompter.ask(prompt).await? {
            PromptResponse::Secret { value, remember } => Ok((value, remember)),
            _ => Err(Error::Cancelled),
        }
    }

    async fn try_password(&mut self, password: String) -> Result<Step> {
        let user = self.target.user.clone();
        let res = net(
            self.timeout,
            self.target,
            self.handle.authenticate_password(user, password),
        )
        .await?;
        Ok(self.record(res))
    }
}

/// Where the answer to a password-like keyboard-interactive round came from.
enum KbdPassword {
    Stored,
    Typed { value: String, remember: bool },
}

/// A keyboard-interactive round that is really a password prompt: a single
/// hidden prompt mentioning "password" (e.g. PAM's `Password: `).
fn is_password_round(prompts: &[russh::client::Prompt]) -> bool {
    match prompts {
        [only] => !only.echo && only.prompt.to_lowercase().contains("password"),
        _ => false,
    }
}

/// Bounds a single request/response exchange with the server (never a
/// prompt to the user).
async fn net<T>(
    timeout: Duration,
    target: &ConnectTarget,
    fut: impl Future<Output = Result<T, russh::Error>>,
) -> Result<T> {
    match tokio::time::timeout(timeout, fut).await {
        Ok(res) => Ok(res?),
        Err(_) => Err(timeout_error(target)),
    }
}

fn timeout_error(target: &ConnectTarget) -> Error {
    Error::Timeout(format!("authentication with {}", target.label))
}

/// Public half of an identity: `<path>.pub`, else the (unencrypted) public
/// part of an OpenSSH private key file.
fn read_public_key(path: &Path) -> Option<PublicKey> {
    let mut pub_path = path.as_os_str().to_owned();
    pub_path.push(".pub");
    if let Ok(key) = russh::keys::load_public_key(&pub_path) {
        return Some(key);
    }
    let text = std::fs::read_to_string(path).ok()?;
    PrivateKey::from_openssh(text.trim())
        .ok()
        .map(|k| k.public_key().clone())
}

/// Key derivation for encrypted keys is deliberately slow; keep it off the
/// async workers.
async fn decrypt(text: &str, passphrase: &str) -> Option<PrivateKey> {
    let (text, passphrase) = (text.to_owned(), passphrase.to_owned());
    tokio::task::spawn_blocking(move || russh::keys::decode_secret_key(&text, Some(&passphrase)))
        .await
        .ok()?
        .ok()
}

// Keychain access is blocking and may be unavailable (headless Linux);
// failures are logged and treated as "nothing stored".

async fn secret_get(account: &str) -> Option<String> {
    let account = account.to_owned();
    match tokio::task::spawn_blocking(move || secrets::get(&account)).await {
        Ok(Ok(secret)) => secret,
        Ok(Err(e)) => {
            tracing::debug!("keychain lookup failed: {e}");
            None
        }
        Err(e) => {
            tracing::debug!("keychain lookup panicked: {e}");
            None
        }
    }
}

async fn secret_set(account: &str, secret: &str) {
    let (account, secret) = (account.to_owned(), secret.to_owned());
    match tokio::task::spawn_blocking(move || secrets::set(&account, &secret)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("could not store secret in keychain: {e}"),
        Err(e) => tracing::warn!("keychain store panicked: {e}"),
    }
}

async fn secret_delete(account: &str) {
    let account = account.to_owned();
    match tokio::task::spawn_blocking(move || secrets::delete(&account)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::debug!("could not delete secret from keychain: {e}"),
        Err(e) => tracing::debug!("keychain delete panicked: {e}"),
    }
}
