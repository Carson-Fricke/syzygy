//! A shared per-user grant for the CLI and other local runtime hosts.

use crate::{ChatGpt, HostId, Session};
use reqwest::Url;
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};
use syzygy_runtime_storage::replace_projection;

type AccountResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone)]
pub struct AccountStore {
    directory: PathBuf,
}

impl AccountStore {
    pub fn default_location() -> io::Result<Self> {
        let home = env::var_os("SYZYGY_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::home_dir().map(|p| p.join(".syzygy")))
            .ok_or_else(|| io::Error::other("set SYZYGY_HOME to a private directory"))?;
        Self::at(home)
    }

    pub fn at(directory: impl Into<PathBuf>) -> io::Result<Self> {
        let directory = directory.into();
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&directory)?.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Syzygy data directory must be private (mode 0700)",
                ));
            }
        }
        Ok(Self { directory })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn lock(&self) -> io::Result<fs::File> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join("account.lock"))?;
        file.lock()?;
        Ok(file)
    }

    fn stored(&self) -> AccountResult<Option<Session>> {
        match fs::read(self.directory.join("account.grant")) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&unprotect(&bytes)?)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self, session: &Session) -> AccountResult<()> {
        let protected = protect(&serde_json::to_vec(session)?)?;
        Ok(replace_projection(
            &self.directory.join("account.grant"),
            &protected,
        )?)
    }

    pub fn sign_in(
        &self,
        client: &ChatGpt,
        launch: impl FnOnce(&Url) -> io::Result<()>,
    ) -> AccountResult<()> {
        let _guard = self.lock()?;
        let path = self.directory.join("host.json");
        let host: HostId = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let host = HostId::default();
                replace_projection(&path, &serde_json::to_vec(&host)?)?;
                host
            }
            Err(error) => return Err(error.into()),
        };
        let existing = self.stored()?;
        let session = client.sign_in(&host, existing.as_ref(), launch)?;
        self.save(&session)
    }

    /// A process refreshes the latest stored grant while holding the account
    /// lock, then releases it before the inference request starts.
    pub fn session(&self, client: &ChatGpt) -> AccountResult<Session> {
        let _guard = self.lock()?;
        let mut session = self.stored()?.ok_or("Sign in with `syzygy login` first")?;
        if session.needs_refresh() {
            client.refresh(&mut session)?;
            self.save(&session)?;
        }
        Ok(session)
    }

    /// Identify the verified account for offline projection without refreshing it.
    pub fn replay_scope(&self) -> AccountResult<String> {
        let _guard = self.lock()?;
        Ok(self
            .stored()?
            .ok_or("Sign in with `syzygy login` first")?
            .replay_scope())
    }

    pub fn forget(&self) -> io::Result<()> {
        let _guard = self.lock()?;
        match fs::remove_file(self.directory.join("account.grant")) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

#[cfg(unix)]
fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    Ok(bytes.to_vec())
}
#[cfg(unix)]
use protect as unprotect;

#[cfg(windows)]
fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    dpapi(bytes, true)
}
#[cfg(windows)]
fn unprotect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    dpapi(bytes, false)
}

#[cfg(windows)]
fn dpapi(bytes: &[u8], encrypt: bool) -> io::Result<Vec<u8>> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes
            .len()
            .try_into()
            .map_err(|_| io::Error::other("credential is too large"))?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // Both APIs read the input for the duration of the call and allocate
    // successful output using LocalAlloc. No machine-wide protection flag is set.
    unsafe {
        let ok = if encrypt {
            CryptProtectData(
                &input,
                null(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        Ok(result)
    }
}
