use std::{
    env,
    fmt::Display,
    fs::File,
    io::{Read, Write},
    os::unix::{net::UnixStream, prelude::AsRawFd},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result};
use indoc::indoc;
use mio::{unix::SourceFd, Events, Interest, Poll, Token};
use nix::{
    fcntl::{flock, FlockArg},
    sys::stat::{umask, Mode},
    unistd::mkfifo,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

fn find_ipc_path() -> Option<PathBuf> {
    let base = PathBuf::from(
        ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
            .into_iter()
            .find_map(|v| env::var(v).ok())
            .unwrap_or_else(|| "/tmp".to_owned()),
    );
    (0..10).find_map(|n| base.join(format!("discord-ipc-{n}")).canonicalize().ok())
}

fn get_merge_list_length() -> u32 {
    Command::new("python")
        .arg("-c")
        .arg(indoc! {"
            import portage
            l = portage.mtimedb.get('resume', {}).get('mergelist')
            print(0 if l is None else len(l), end='')
        "})
        .output()
        .ok()
        .and_then(|out| std::str::from_utf8(&out.stdout).ok()?.parse().ok())
        .unwrap_or(0)
}

pub struct Client {
    client_id: String,
    stream: Option<UnixStream>,
    path: Option<PathBuf>,
    merge_len: Option<u32>,
}

impl Client {
    pub fn new(id: &(impl ToString + ?Sized)) -> Self {
        Self {
            client_id: id.to_string(),
            stream: None,
            path: None,
            merge_len: None,
        }
    }
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
    fn handle_io(&mut self, io: std::io::Result<()>) -> Result<()> {
        match io {
            Err(io) => match io.kind() {
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset => {
                    if let Some(stream) = self.stream.as_mut() {
                        stream.shutdown(std::net::Shutdown::Both).ok();
                        self.stream = None;
                        Err(anyhow::anyhow!("Broken Pipe"))
                    } else {
                        Ok(())
                    }
                }
                _ => Err(io)?,
            },
            Ok(()) => Ok(()),
        }
    }
    pub fn connect(&mut self) -> Result<()> {
        log::trace!("Connect");
        if !self.is_connected() {
            self.path = Some(find_ipc_path().context("Couldn't find discord-ipc")?);
            self.stream = UnixStream::connect(self.path.as_ref().unwrap()).ok();
            self.stream.as_ref().context("Failed to connect")?;
            log::trace!("Connected");
            self.handshake()?;
        }
        Ok(())
    }
    fn nonce(&self) -> String {
        format!("{:016x}", rand::random::<u128>())
    }
    pub fn send(&mut self, opcode: u32, payload: &impl Serialize) -> Result<()> {
        let stream = self.stream.as_mut().context("Socket isn't open")?;
        let mut buf = Vec::new();
        let payload = serde_json::to_string(payload)?;
        let len = payload.len() as u32;
        buf.extend_from_slice(&opcode.to_le_bytes());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(payload.as_bytes());
        let res = stream.write_all(&buf);
        self.handle_io(res)?;
        log::trace!("Sent opcode {opcode} with payload: {payload}");
        Ok(())
    }
    pub fn recv(&mut self) -> Result<(u32, String)> {
        let stream = self.stream.as_mut().context("Socket isn't open")?;
        let opcode = get_number(stream)?;
        let len = get_number(stream)?;
        let mut buf = vec![0u8; len as usize];
        let res = stream.read_exact(&mut buf);
        self.handle_io(res)?;
        let payload = std::str::from_utf8(&buf)?.to_string();
        log::trace!("Received opcode {opcode} with payload {payload}");
        Ok((opcode, payload))
    }
    pub fn handshake(&mut self) -> Result<()> {
        let res = self.send(
            0,
            &json!({
                "v": 1u32,
                "client_id": self.client_id,
                "nonce": self.nonce(),
            }),
        );
        log::debug!("Handshake response: {:?}", self.recv());
        res
    }
    pub fn reconnect(&mut self) -> Result<()> {
        log::trace!("Reconnection");

        self.send(2, &json!({})).ok();
        if let Some(stream) = self.stream.as_mut() {
            log::trace!("Sent disconnection");
            stream.flush()?;
            stream.shutdown(std::net::Shutdown::Both).ok();
            log::trace!("Socket shutdown (flush)");
        }

        self.path = Some(find_ipc_path().context("Couldn't find discord-ipc")?);
        self.stream = UnixStream::connect(self.path.as_ref().unwrap()).ok();
        self.stream.as_ref().context("Reconnection failed")?;

        log::trace!("New connection open");
        self.handshake()?;

        Ok(())
    }

    pub fn set_package(&mut self, payload: PackagePayload) -> Result<()> {
        let count = get_merge_list_length();
        log::trace!("Got merge list len: {count}");
        let new_count = self.merge_len.unwrap_or(0).max(count);
        let party = match new_count {
            0 => None,
            1.. => Some(json!({
                "id": "id",
                "size": [new_count - count + 1, new_count]
            })),
        };

        self.merge_len = Some(new_count);

        let PackagePayload {
            category, package, ..
        } = payload;

        let mut value = json!({
            "details": format!("{category}/{package}"),
            "timestamps": {
                "start": SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis() as u64,
            },
            "assets": {
                "large_image": "gentoodrpgt"
            },
        });

        if let Some(state) = payload.state {
            value
                .as_object_mut()
                .unwrap()
                .insert("state".to_owned(), json!(state));
        }
        if let Some(party) = party {
            value
                .as_object_mut()
                .unwrap()
                .insert("party".to_owned(), party);
        }

        self.send(
            1,
            &json!({
                "cmd": "SET_ACTIVITY",
                "nonce": self.nonce(),
                "args": {
                    "activity": value,
                    "pid": 0u32
                }
            }),
        )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum PackageState {
    Preparing,
    Compiling,
    Installing,
}

impl Display for PackageState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preparing => write!(f, "preparing"),
            Self::Compiling => write!(f, "compiling"),
            Self::Installing => write!(f, "installing"),
        }
    }
}

#[derive(Deserialize)]
pub struct PackagePayload {
    category: String,
    package: String,
    state: Option<PackageState>,
}

pub fn get_number(stream: &mut UnixStream) -> Result<u32> {
    let mut buf = [0u8; 4];
    let len = stream.read(&mut buf)?;
    if len < 4 {
        Err(anyhow::anyhow!("Not enough bytes"))
    } else {
        Ok(u32::from_le_bytes(buf))
    }
}

fn run(
    client: &mut Client,
    file: &mut File,
    buf: &mut Vec<u8>,
    poll: &mut Poll,
    last_unset: &mut Option<Instant>,
) -> Result<()> {
    let mut events = Events::with_capacity(1);
    poll.poll(&mut events, Some(Duration::from_secs(5)))?;
    let len = file.read_to_end(buf)?;

    if !client.is_connected() {
        return client.connect();
    }

    if len > 0 {
        log::info!("Received command");
        let str = std::str::from_utf8(buf)?;
        let commands = str.split_terminator('\u{0}').collect::<Vec<_>>();
        log::trace!(
            "Got {} commands, evaluating last ({commands:?})",
            commands.len()
        );
        if let Some(command) = commands.last() {
            if command.starts_with("set ") {
                log::info!("Got set");
                let json = command
                    .strip_prefix("set ")
                    .context("Command is missing arguments")?;
                let val: PackagePayload = serde_json::from_str(json)?;
                client.set_package(val)?;
                log::info!("Response: {:?}", client.recv());
                *last_unset = None;
            } else if command.starts_with("unset") {
                log::info!("Got unset, queueing");
                *last_unset = Some(Instant::now());
            }
        }

        buf.clear();
    }

    if let Some(ts) = last_unset {
        if ts.elapsed() > Duration::from_secs(30) {
            log::info!("30 seconds has passed since the last unset with  no further commands, reconnecting.");
            client.reconnect()?;
            client.merge_len = None;
            *last_unset = None;
        }
    }

    Ok(())
}

const PIPE: Token = Token(0);

fn main() {
    //TODO: Daemonize
    env_logger::init();
    log::info!("Starting");

    // Create the file if needed
    let mut pid_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open("/tmp/rpcdiscordpid")
        .expect("Couldn't open pid file");
    flock(pid_file.as_raw_fd(), FlockArg::LockExclusiveNonblock)
        .expect("Couldn't lock pid file, another process may be using it");
    pid_file
        .write_all(std::process::id().to_string().as_bytes())
        .expect("Failed to write pid");

    if !Path::new("/tmp/_discordfifo").exists() {
        log::info!("No fifo found, creating it");
        // Otherwise pipe is created as prw-r--r--
        let prev = umask(Mode::empty());
        mkfifo(
            "/tmp/_discordfifo",
            Mode::S_IRUSR
                | Mode::S_IWUSR
                | Mode::S_IRGRP
                | Mode::S_IWGRP
                | Mode::S_IROTH
                | Mode::S_IWOTH,
        )
        .unwrap();
        umask(prev);
    }
    let mut client = Client::new("1007427345801556039");
    match client.connect() {
        Ok(()) => log::info!("Client connected"),
        Err(err) => log::warn!("Connection failed ({err:?})"),
    }
    let mut buf = Vec::new();
    let mut poll = Poll::new().unwrap();
    let mut file = File::options()
        .read(true)
        .write(false)
        .open("/tmp/_discordfifo")
        .unwrap();
    poll.registry()
        .register(&mut SourceFd(&file.as_raw_fd()), PIPE, Interest::READABLE)
        .unwrap();
    let mut last_unset = None;
    loop {
        log::info!("Waiting for command");
        match run(&mut client, &mut file, &mut buf, &mut poll, &mut last_unset) {
            Ok(()) => {}
            Err(e) => log::warn!("{e:?}"),
        }
    }
}
