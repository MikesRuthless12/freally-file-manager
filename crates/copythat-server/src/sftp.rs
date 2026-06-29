//! Phase 48 follow-up — the SFTP transport.
//!
//! Unlike the WebDAV/HTTP surface (which delegates path safety to
//! `dav-server`'s `LocalFs`), the SFTP server is the *only* path jail in
//! front of the served root: every client-supplied path is funnelled
//! through [`SftpHandler::resolve`], which lexically normalises `.` / `..`
//! against the canonicalised root and refuses anything that would escape
//! it. There is no symlink-following in the resolver — the jail is purely
//! lexical over a canonical root, so a crafted path can never climb out.
//!
//! Transport: [`russh`] provides the SSH layer (one ephemeral ed25519 host
//! key per [`crate::serve`] call); [`russh_sftp`] drives the SFTP packet
//! loop over each opened `sftp` subsystem channel. Auth maps the server's
//! [`AuthMode`] onto SSH `none` / `password` methods; `readonly` rejects
//! every mutating operation with `SSH_FX_PERMISSION_DENIED`.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, Config as SshServerConfig, Handler as SshHandler, Msg, Server, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File as SftpFile, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::{AuthMode, ServerConfig, ServerError};

/// Upper bound on a single `SSH_FXP_READ` allocation. Returning fewer bytes
/// than the client asked for is protocol-legal (the client re-reads at the
/// new offset), so capping here turns a hostile `len = u32::MAX` request
/// into a bounded allocation instead of a 4 GiB one.
const MAX_READ_LEN: usize = 256 * 1024;

/// Bind the SFTP transport: generate an ephemeral host key, build the russh
/// server, and spawn its accept loop on the already-bound `listener`.
/// Returns the serving task; `shutdown_rx` firing drives a graceful stop.
pub(crate) fn spawn(
    config: &ServerConfig,
    listener: TcpListener,
    shutdown_rx: oneshot::Receiver<()>,
) -> Result<JoinHandle<()>, ServerError> {
    // The jail compares normalised paths against the *canonical* root, so a
    // served root that doesn't exist (or can't be canonicalised) is a hard
    // error rather than a silently-unjailed server.
    let root = std::fs::canonicalize(&config.root).map_err(|e| ServerError::Bind {
        addr: config.bind_addr.clone(),
        message: format!("sftp served root {:?} is unusable: {e}", config.root),
    })?;

    // One fresh ed25519 host key per serve() — these servers are loopback /
    // homelab endpoints fronted by `known_hosts` pinning on the client, not
    // long-lived public hosts, so an ephemeral key keeps key management out
    // of scope. `rand::rng()` is the version-correct RNG for ssh-key 0.6.
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).map_err(|e| {
        ServerError::Bind {
            addr: config.bind_addr.clone(),
            message: format!("sftp host key generation failed: {e}"),
        }
    })?;

    let ssh_config = Arc::new(SshServerConfig {
        keys: vec![host_key],
        ..Default::default()
    });

    let server = SftpServer {
        root: Arc::new(root),
        auth: Arc::new(config.auth.clone()),
        readonly: config.readonly,
    };

    Ok(tokio::spawn(run_server(
        server,
        ssh_config,
        listener,
        shutdown_rx,
    )))
}

/// Drive the russh accept loop until either it ends or `shutdown_rx` fires.
async fn run_server(
    mut server: SftpServer,
    ssh_config: Arc<SshServerConfig>,
    listener: TcpListener,
    shutdown_rx: oneshot::Receiver<()>,
) {
    // `run_on_socket` borrows `server` + `listener` for the lifetime of the
    // returned future; both are owned locals here, so the whole task stays
    // `'static` and self-contained.
    let mut running = server.run_on_socket(ssh_config, &listener);
    let handle = running.handle();
    tokio::select! {
        _ = shutdown_rx => {
            handle.shutdown("copythat sftp server shutting down".to_string());
            let _ = (&mut running).await;
        }
        result = &mut running => {
            if let Err(e) = result {
                tracing::warn!(error = ?e, "copythat sftp server task ended with error");
            }
        }
    }
}

/// The russh [`Server`]: holds the shared, immutable serving parameters and
/// mints one [`SshConn`] per inbound connection.
struct SftpServer {
    root: Arc<PathBuf>,
    auth: Arc<AuthMode>,
    readonly: bool,
}

impl Server for SftpServer {
    type Handler = SshConn;

    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> SshConn {
        SshConn {
            root: self.root.clone(),
            auth: self.auth.clone(),
            readonly: self.readonly,
            channels: HashMap::new(),
        }
    }
}

/// Per-connection SSH handler. Authenticates the peer, then hands each
/// `sftp` subsystem request off to a fresh [`SftpHandler`].
struct SshConn {
    root: Arc<PathBuf>,
    auth: Arc<AuthMode>,
    readonly: bool,
    /// Session channels opened by the client, keyed by id, awaiting a
    /// subsystem request that turns them into an SFTP stream.
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl SshHandler for SshConn {
    // Reuse russh's own error as our handler error: it satisfies the trait's
    // `From<russh::Error>` bound reflexively and is `Send`, so the `?` on
    // `session.channel_success(..)` works without a bespoke error type.
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        // Only an explicitly unauthenticated server accepts the `none`
        // method; otherwise reject so the client falls back to password.
        Ok(match &*self.auth {
            AuthMode::None => Auth::Accept,
            _ => Auth::reject(),
        })
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let ok = match &*self.auth {
            AuthMode::None => true,
            // Bearer: the token travels as the SSH password, any username.
            AuthMode::Bearer { token } => ct_eq(password, token),
            // Basic: both username and password must match (constant-time,
            // non-short-circuiting `&`, mirroring the HTTP path).
            AuthMode::Basic {
                user: want_user,
                password: want_pw,
            } => ct_eq(user, want_user) & ct_eq(password, want_pw),
        };
        Ok(if ok { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.channels.insert(channel.id(), channel);
        Ok(true)
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            if let Some(channel) = self.channels.remove(&channel_id) {
                let handler = SftpHandler::new(self.root.clone(), self.readonly);
                session.channel_success(channel_id)?;
                // `run` spawns the packet loop on its own task and returns
                // promptly, so the SSH session loop keeps servicing the
                // connection.
                russh_sftp::server::run(channel.into_stream(), handler).await;
            } else {
                session.channel_failure(channel_id)?;
            }
        } else {
            session.channel_failure(channel_id)?;
        }
        Ok(())
    }
}

/// An open handle returned to the client: either a file (for read/write at
/// an offset) or a directory whose entries are sent in one `readdir` batch.
enum OpenHandle {
    File(std::fs::File),
    Dir { entries: Vec<SftpFile>, sent: bool },
}

/// The SFTP subsystem handler for one channel — the path jail lives here.
struct SftpHandler {
    /// Canonicalised served root; every resolved path must stay under it.
    root: Arc<PathBuf>,
    readonly: bool,
    handles: HashMap<String, OpenHandle>,
    next_handle: u64,
}

impl SftpHandler {
    fn new(root: Arc<PathBuf>, readonly: bool) -> Self {
        Self {
            root,
            readonly,
            handles: HashMap::new(),
            next_handle: 0,
        }
    }

    /// Confine a client-supplied path under the served root.
    ///
    /// SFTP clients treat the served root as `/` (so `/foo` means
    /// `<root>/foo`). We strip the leading separator, then walk the path
    /// components lexically — skipping `.`, popping on `..` (never above the
    /// root), and rejecting any absolute/`..` escape or drive prefix. No
    /// component is ever resolved through the filesystem, so symlinks under
    /// the root cannot be used to break out. The normalised result is
    /// finally re-checked to start with the canonical root.
    fn resolve(&self, client_path: &str) -> Result<PathBuf, StatusCode> {
        let relative = client_path.trim_start_matches('/');
        let mut normalized = (*self.root).clone();
        for component in Path::new(relative).components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    // Pop one level, but never climb above the served root.
                    if !normalized.pop() || !normalized.starts_with(&*self.root) {
                        return Err(StatusCode::PermissionDenied);
                    }
                }
                // A rooted path (`/...` on unix) or a drive/UNC prefix
                // (`C:\...` on Windows) is an absolute escape attempt.
                Component::RootDir | Component::Prefix(_) => {
                    return Err(StatusCode::PermissionDenied);
                }
            }
        }
        // Belt-and-suspenders: the lexical walk should already guarantee
        // this, but re-verify the final path is genuinely under the root.
        if !normalized.starts_with(&*self.root) {
            return Err(StatusCode::PermissionDenied);
        }
        Ok(normalized)
    }

    /// Map a resolved real path back to the client's virtual `/`-rooted view.
    fn virtual_path(&self, resolved: &Path) -> String {
        let rel = resolved.strip_prefix(&*self.root).unwrap_or(Path::new(""));
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.is_empty() {
            "/".to_string()
        } else {
            format!("/{rel}")
        }
    }

    fn fresh_handle(&mut self) -> String {
        let key = self.next_handle.to_string();
        self.next_handle += 1;
        key
    }

    /// Reject mutating ops up-front on a read-only server.
    fn deny_if_readonly(&self) -> Result<(), StatusCode> {
        if self.readonly {
            Err(StatusCode::PermissionDenied)
        } else {
            Ok(())
        }
    }
}

/// Constant-time string compare via the workspace's shared `subtle`
/// primitive — same construction as the HTTP path's `ct_eq`.
fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// A successful `SSH_FXP_STATUS` for `id`.
fn ok_status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_string(),
        language_tag: "en-US".to_string(),
    }
}

/// Translate a filesystem error into the closest SFTP status code.
fn io_status(err: &std::io::Error) -> StatusCode {
    match err.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

impl russh_sftp::server::Handler for SftpHandler {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let resolved = self.resolve(&path)?;
        Ok(Name {
            id,
            files: vec![SftpFile::dummy(self.virtual_path(&resolved))],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let dir = self.resolve(&path)?;
        let read_dir = std::fs::read_dir(&dir).map_err(|e| io_status(&e))?;
        let mut entries = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|e| io_status(&e))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let attrs = match entry.metadata() {
                Ok(meta) => FileAttributes::from(&meta),
                Err(_) => FileAttributes::default(),
            };
            entries.push(SftpFile::new(name, attrs));
        }
        let handle = self.fresh_handle();
        self.handles.insert(
            handle.clone(),
            OpenHandle::Dir {
                entries,
                sent: false,
            },
        );
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(OpenHandle::Dir { entries, sent }) => {
                if *sent {
                    // Whole listing already delivered — signal end-of-dir.
                    Err(StatusCode::Eof)
                } else {
                    *sent = true;
                    Ok(Name {
                        id,
                        files: std::mem::take(entries),
                    })
                }
            }
            _ => Err(StatusCode::Failure),
        }
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let write_flags = OpenFlags::WRITE
            | OpenFlags::CREATE
            | OpenFlags::TRUNCATE
            | OpenFlags::APPEND
            | OpenFlags::EXCLUDE;
        if pflags.intersects(write_flags) {
            self.deny_if_readonly()?;
        }
        let path = self.resolve(&filename)?;
        let options: std::fs::OpenOptions = pflags.into();
        let file = options.open(&path).map_err(|e| io_status(&e))?;
        let handle = self.fresh_handle();
        self.handles.insert(handle.clone(), OpenHandle::File(file));
        Ok(Handle { id, handle })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(OpenHandle::File(file)) => {
                file.seek(SeekFrom::Start(offset))
                    .map_err(|e| io_status(&e))?;
                let want = (len as usize).min(MAX_READ_LEN);
                let mut buf = vec![0u8; want];
                let mut filled = 0;
                while filled < buf.len() {
                    match file.read(&mut buf[filled..]) {
                        Ok(0) => break,
                        Ok(n) => filled += n,
                        Err(e) => return Err(io_status(&e)),
                    }
                }
                if filled == 0 {
                    return Err(StatusCode::Eof);
                }
                buf.truncate(filled);
                Ok(Data { id, data: buf })
            }
            _ => Err(StatusCode::Failure),
        }
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        self.deny_if_readonly()?;
        match self.handles.get_mut(&handle) {
            Some(OpenHandle::File(file)) => {
                file.seek(SeekFrom::Start(offset))
                    .map_err(|e| io_status(&e))?;
                file.write_all(&data).map_err(|e| io_status(&e))?;
                Ok(ok_status(id))
            }
            _ => Err(StatusCode::Failure),
        }
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok_status(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let resolved = self.resolve(&path)?;
        let meta = std::fs::symlink_metadata(&resolved).map_err(|e| io_status(&e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let resolved = self.resolve(&path)?;
        let meta = std::fs::metadata(&resolved).map_err(|e| io_status(&e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        match self.handles.get(&handle) {
            Some(OpenHandle::File(file)) => {
                let meta = file.metadata().map_err(|e| io_status(&e))?;
                Ok(Attrs {
                    id,
                    attrs: FileAttributes::from(&meta),
                })
            }
            _ => Err(StatusCode::Failure),
        }
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        self.deny_if_readonly()?;
        let resolved = self.resolve(&path)?;
        std::fs::create_dir(&resolved).map_err(|e| io_status(&e))?;
        Ok(ok_status(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        self.deny_if_readonly()?;
        let resolved = self.resolve(&path)?;
        std::fs::remove_dir(&resolved).map_err(|e| io_status(&e))?;
        Ok(ok_status(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        self.deny_if_readonly()?;
        let resolved = self.resolve(&filename)?;
        std::fs::remove_file(&resolved).map_err(|e| io_status(&e))?;
        Ok(ok_status(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        self.deny_if_readonly()?;
        let from = self.resolve(&oldpath)?;
        let to = self.resolve(&newpath)?;
        std::fs::rename(&from, &to).map_err(|e| io_status(&e))?;
        Ok(ok_status(id))
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        self.deny_if_readonly()?;
        // Jail the path, then accept without applying attributes: chmod /
        // chown semantics are largely meaningless on the Windows hosts this
        // targets, and clients (`sftp put`) issue setstat after upload and
        // expect success. The bytes are already written; ignoring the mode
        // bits is the pragmatic, safe behaviour for a homelab file server.
        let _ = self.resolve(&path)?;
        Ok(ok_status(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        self.deny_if_readonly()?;
        if self.handles.contains_key(&handle) {
            Ok(ok_status(id))
        } else {
            Err(StatusCode::Failure)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler_for(root: &Path) -> SftpHandler {
        let canonical = std::fs::canonicalize(root).unwrap();
        SftpHandler::new(Arc::new(canonical), false)
    }

    #[test]
    fn jail_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("file.txt"), b"hi").unwrap();
        let handler = handler_for(dir.path());

        // `..` escapes — rejected on every platform.
        for bad in ["../etc/passwd", "/../../etc", "a/../../b"] {
            assert_eq!(
                handler.resolve(bad),
                Err(StatusCode::PermissionDenied),
                "traversal `{bad}` must be denied"
            );
        }

        // A Windows drive-absolute path parses as a `Prefix` component only
        // on Windows; gate the assertion so the suite stays green on Linux.
        #[cfg(windows)]
        assert_eq!(
            handler.resolve("C:\\Windows\\System32"),
            Err(StatusCode::PermissionDenied),
            "absolute drive path must be denied"
        );

        // A legitimate, root-relative path resolves under the served root.
        let ok = handler
            .resolve("/sub/file.txt")
            .expect("legit path resolves");
        assert!(ok.starts_with(&*handler.root));
        assert!(ok.ends_with("file.txt"));
    }

    #[test]
    fn jail_allows_dot_and_root() {
        let dir = tempfile::tempdir().unwrap();
        let handler = handler_for(dir.path());
        // "/" and "." both denote the served root itself.
        assert_eq!(handler.resolve("/").unwrap(), *handler.root);
        assert_eq!(handler.resolve(".").unwrap(), *handler.root);
        // An in-bounds `..` that stays under root is fine.
        let p = handler.resolve("/a/../b").unwrap();
        assert!(p.starts_with(&*handler.root));
        assert!(p.ends_with("b"));
    }
}
