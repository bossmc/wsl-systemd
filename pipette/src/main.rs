use std::io::Read as _;
use std::io::Write as _;
use std::sync::Arc;

#[derive(structopt::StructOpt, Debug)]
struct Args {
    #[structopt(subcommand)]
    mode: Mode,
}

#[derive(structopt::StructOpt, Debug)]
enum Mode {
    GpgAgent,
}

fn main() {
    let args = <Args as structopt::StructOpt>::from_args();
    eprintln!("{:?}", args);

    let sock = match args.mode {
        Mode::GpgAgent => {
            let dirs = directories::BaseDirs::new().unwrap();
            let app_data = dirs.data_local_dir();
            let gnupg_data = app_data.join("gnupg");
            let assuan = gnupg_data.join("S.gpg-agent");
            assuan::Assuan::new(&assuan).unwrap()
        }
    };
    attach_to_tty(sock);
}

trait Split {
    type Read: Send + Sync + 'static;
    type Write: Send + Sync + 'static;
    fn split(self) -> (Arc<Self::Read>, Arc<Self::Write>);
}

fn attach_to_tty<S: Split>(splittable: S)
where
    for<'a> &'a S::Read: std::io::Read,
    for<'a> &'a S::Write: std::io::Write,
{
    let (read, write) = splittable.split();
    let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let bob = std::thread::spawn({
        let terminated = Arc::clone(&terminated);
        move || loop {
            if terminated.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let mut buf = [0; 128];
            match read.as_ref().read(&mut buf) {
                Ok(0) => {
                    eprintln!("sock closed");
                    std::process::exit(0);
                }
                Ok(len) => {
                    std::io::stdout().write_all(&buf[..len]).unwrap();
                }
                Err(e) => eprintln!("{}", e),
            };
        }
    });
    let fred = std::thread::spawn(move || loop {
        if terminated.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let mut buf = [0; 128];
        match std::io::stdin().read(&mut buf) {
            Ok(0) => {
                eprintln!("stdin closed");
                std::process::exit(0);
            }
            Ok(len) => {
                write.as_ref().write_all(&buf[..len]).unwrap();
            }
            Err(e) => panic!("{}", e),
        };
    });
    bob.join().unwrap();
    fred.join().unwrap();
}

mod assuan {
    use std::io::BufRead as _;
    use std::io::Read as _;
    use std::io::Write as _;
    use std::sync::Arc;

    #[derive(thiserror::Error, Debug)]
    pub enum Error {
        #[error("An IO Error occurred opening the Assuan file")]
        IO(#[from] std::io::Error),
        #[error("Failed to parse port from the Assan file")]
        PortParse(#[from] std::num::ParseIntError),
        #[error("Failed to parse nonce from the Assuan file")]
        NonceParse,
    }

    pub struct Assuan {
        sock: std::net::TcpStream,
    }

    impl Assuan {
        pub fn new(path: &std::path::Path) -> Result<Self, Error> {
            // Open the Assuan file
            eprintln!("Opening {}", path.display());
            let data_file = std::fs::File::open(path)?;
            let mut data_file = std::io::BufReader::new(data_file);

            // Format is:
            //
            // ```text
            // aaaa
            // bbbbbbbbbbbbbbbb
            // ```
            //
            // Where `aaaa` is the port on localhost to connect to and `bbbbbbbbbbbb` is a 16-byte
            // nonce to authenticate the connection.
            let mut port = String::new();
            data_file.read_line(&mut port)?;
            let mut nonce = [0u8; 16];
            let nonce_len = data_file.read(&mut nonce)?;
            if nonce_len != 16 {
                return Err(Error::NonceParse);
            }
            let port: u16 = port.trim().parse()?;

            eprintln!(
                "Discovered assuan socket at 127.0.0.1:{} ({:?})",
                port, nonce
            );

            let mut sock = std::net::TcpStream::connect(("127.0.0.1", port))?;
            sock.write_all(&nonce[..])?;

            Ok(Self { sock })
        }
    }

    impl super::Split for Assuan {
        type Read = std::net::TcpStream;
        type Write = std::net::TcpStream;
        fn split(self) -> (Arc<Self::Read>, Arc<Self::Write>) {
            let arc = Arc::new(self.sock);
            (Arc::clone(&arc) as Arc<_>, arc as Arc<_>)
        }
    }
}
