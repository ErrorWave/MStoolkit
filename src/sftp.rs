// SFTP worker — pure-Rust SSH via russh + russh-sftp.
// Runs as a tokio task; communicates with the UI via unbounded channels.

use anyhow::anyhow;
use russh::client;
use russh_keys::key::PublicKey;
use russh_sftp::client::SftpSession;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SftpEntry {
    pub name: String,
    pub full_path: String,
    pub is_dir: bool,
    pub size: u64,
}

pub enum SftpCmd {
    ListDir(String),
    Download { path: String, name: String },
    Disconnect,
}

pub enum SftpEvent {
    Connected { home_dir: String },
    DirListing { path: String, entries: Vec<SftpEntry> },
    FileData { name: String, data: Vec<u8> },
    Busy(bool),
    Error(String),
}

// ── SSH handler (accept all host keys) ───────────────────────────────────────

struct NoVerifyHandler;

#[async_trait::async_trait]
impl client::Handler for NoVerifyHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        // NOTE: in production you'd verify the host key.
        Ok(true)
    }
}

// ── Worker task ───────────────────────────────────────────────────────────────

pub async fn worker_task(
    host: String,
    port: u16,
    username: String,
    password: String,
    mut cmd_rx: UnboundedReceiver<SftpCmd>,
    evt_tx: UnboundedSender<SftpEvent>,
    repaint: impl Fn() + Send + 'static,
) {
    let result: Result<(), anyhow::Error> = async {
        // ── TCP + SSH ─────────────────────────────────────────────────────────
        let config = Arc::new(client::Config::default());
        let mut session =
            client::connect(config, (host.as_str(), port), NoVerifyHandler)
                .await
                .map_err(|e| anyhow!("Connect to {host}:{port}: {e}"))?;

        let auth = session
            .authenticate_password(username, password)
            .await
            .map_err(|e| anyhow!("Auth: {e}"))?;
        if !auth {
            return Err(anyhow!("Authentication failed (wrong credentials)"));
        }

        // ── SFTP subsystem ────────────────────────────────────────────────────
        let channel = session
            .channel_open_session()
            .await
            .map_err(|e| anyhow!("Open session channel: {e}"))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| anyhow!("Request sftp subsystem: {e}"))?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| anyhow!("SFTP session: {e}"))?;

        // Resolve working directory (fall back to "/" on failure)
        let home = sftp.canonicalize(".").await.unwrap_or_else(|_| "/".into());
        let _ = evt_tx.send(SftpEvent::Connected { home_dir: home });
        repaint();

        // ── Command loop ──────────────────────────────────────────────────────
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                SftpCmd::ListDir(path) => {
                    let _ = evt_tx.send(SftpEvent::Busy(true));
                    repaint();

                    match sftp.read_dir(&path).await {
                        Ok(dir) => {
                            let mut entries: Vec<SftpEntry> = Vec::new();
                            // read_dir returns a plain iterator in russh-sftp 2.x
                            for entry in dir {
                                let name: String = entry.file_name().to_string();
                                // Skip . and .. to keep the listing clean
                                if name == "." || name == ".." {
                                    continue;
                                }
                                let md = entry.metadata();
                                let is_dir = md.is_dir();
                                let size = md.size.unwrap_or(0);
                                let base = path.trim_end_matches('/');
                                let full_path = format!("{base}/{name}");
                                entries.push(SftpEntry {
                                    name,
                                    full_path,
                                    is_dir,
                                    size,
                                });
                            }
                            // Dirs first, then alphabetical
                            entries.sort_by(|a, b| {
                                b.is_dir
                                    .cmp(&a.is_dir)
                                    .then_with(|| a.name.cmp(&b.name))
                            });
                            let _ = evt_tx.send(SftpEvent::DirListing { path, entries });
                        }
                        Err(e) => {
                            let _ = evt_tx
                                .send(SftpEvent::Error(format!("List {path}: {e}")));
                        }
                    }

                    let _ = evt_tx.send(SftpEvent::Busy(false));
                    repaint();
                }

                SftpCmd::Download { path, name } => {
                    let _ = evt_tx.send(SftpEvent::Busy(true));
                    repaint();

                    let res: Result<Vec<u8>, anyhow::Error> = async {
                        let mut file = sftp
                            .open(&path)
                            .await
                            .map_err(|e| anyhow!("Open {path}: {e}"))?;
                        let mut buf = Vec::new();
                        file.read_to_end(&mut buf)
                            .await
                            .map_err(|e| anyhow!("Read {name}: {e}"))?;
                        Ok(buf)
                    }
                    .await;

                    match res {
                        Ok(data) => {
                            let _ = evt_tx.send(SftpEvent::FileData { name, data });
                        }
                        Err(e) => {
                            let _ = evt_tx
                                .send(SftpEvent::Error(format!("Download {name}: {e}")));
                        }
                    }

                    let _ = evt_tx.send(SftpEvent::Busy(false));
                    repaint();
                }

                SftpCmd::Disconnect => break,
            }
        }

        Ok(())
    }
    .await;

    if let Err(e) = result {
        let _ = evt_tx.send(SftpEvent::Error(e.to_string()));
        repaint();
    }
}
