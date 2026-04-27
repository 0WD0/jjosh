use std::{fs, io, io::prelude::*, path::PathBuf};

fn bash_program() -> io::Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        eprintln!("warning: `bash-program` subcommand not meant for scripting, format may change");
    }
    println!("{}", gix_testtools::bash_program().display());
    Ok(())
}

fn mess_in_the_middle(path: PathBuf) -> io::Result<()> {
    let mut file = fs::OpenOptions::new().read(false).write(true).open(path)?;
    file.seek(io::SeekFrom::Start(file.metadata()?.len() / 2))?;
    file.write_all(b"hello")?;
    Ok(())
}

#[cfg(unix)]
fn umask() -> io::Result<()> {
    println!("{:04o}", gix_testtools::umask());
    Ok(())
}

/// Run a Git protocol test daemon on an OS-assigned loopback port.
///
/// Journey tests use this instead of `git daemon --port=<n>` because Git
/// treats `--port=0` as "use the default port", so it can't bind an
/// ephemeral port and report it back. This wrapper owns the listening socket,
/// writes the resulting `git://127.0.0.1:<port>/` URL to `url_file`, and then
/// hands every accepted connection to `git daemon --inetd`.
#[cfg(unix)]
fn git_daemon(url_file: PathBuf) -> io::Result<()> {
    use std::{
        net::{TcpListener, TcpStream},
        os::fd::{FromRawFd, IntoRawFd},
        process::{Command, Stdio},
        thread,
    };

    fn stream_to_stdio(stream: TcpStream) -> Stdio {
        // SAFETY: `into_raw_fd()` transfers ownership of the socket fd, and `Stdio`
        // takes over closing it in the spawned child.
        unsafe { Stdio::from_raw_fd(stream.into_raw_fd()) }
    }

    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let addr = listener.local_addr()?;
    fs::write(url_file, format!("git://{addr}/\n"))?;

    for incoming in listener.incoming() {
        let stream = incoming?;
        let peer_addr = stream.peer_addr().ok();
        let stdin = stream_to_stdio(stream.try_clone()?);
        let stdout = stream_to_stdio(stream);

        let mut child = Command::new("git")
            .args([
                "-c",
                "uploadpack.allowrefinwant",
                "daemon",
                "--inetd",
                "--verbose",
                "--base-path=.",
                "--export-all",
                "--user-path",
            ])
            .stdin(stdin)
            .stdout(stdout)
            .stderr(Stdio::null())
            .envs(remote_env(peer_addr))
            .spawn()?;

        thread::spawn(move || {
            // Make sure we don't create zombies but wait for all children to finish.
            // Also needed to have our process has handle to all possibly running Git invocations.
            let _ = child.wait();
        });
    }

    Ok(())
}

/// Return the client-address environment Git's own daemon loop would set.
///
/// In `--inetd` mode Git doesn't accept the socket itself, so it can't derive
/// `REMOTE_ADDR` and `REMOTE_PORT`. The wrapper owns `accept()`, and passes
/// these values along for code paths that log or inspect peer information.
/// 
/// As such, it's optional and not needed for it to function.
#[cfg(unix)]
fn remote_env(peer_addr: Option<std::net::SocketAddr>) -> Vec<(&'static str, String)> {
    peer_addr
        .map(|addr| {
            vec![
                ("REMOTE_ADDR", addr.ip().to_string()),
                ("REMOTE_PORT", addr.port().to_string()),
            ]
        })
        .unwrap_or_default()
}

#[cfg(not(unix))]
fn git_daemon(_url_file: PathBuf) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "`jtt git-daemon` is only supported on Unix",
    ))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let scmd = args.next().expect("sub command");
    match &*scmd {
        "bash-program" | "bp" => bash_program()?,
        "git-daemon" => git_daemon(PathBuf::from(args.next().expect("path to write the git:// URL to")))?,
        "mess-in-the-middle" => mess_in_the_middle(PathBuf::from(args.next().expect("path to file to mess with")))?,
        #[cfg(unix)]
        "umask" => umask()?,
        _ => unreachable!("Unknown subcommand: {}", scmd),
    }
    Ok(())
}
