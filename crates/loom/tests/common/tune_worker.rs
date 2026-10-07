//! Environment-isolated worker for real replay processes with an injected logical clock.

use std::future::Future;
use std::time::Duration;

use loom_workflow::tune::{self, ProposeRequest, Request, Response, Surface};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use super::tune_clock::ManualClock;

pub async fn run() {
    let workspace = std::env::var_os("LOOM_TEST_REPLAY_WORKSPACE").expect("worker workspace");
    let hang = std::env::var("LOOM_TEST_REPLAY_HANG")
        .unwrap()
        .parse::<bool>()
        .unwrap();
    let release = std::env::var("LOOM_TEST_REPLAY_RELEASE")
        .unwrap()
        .parse::<bool>()
        .unwrap();
    let wall = std::env::var("LOOM_TEST_REPLAY_WALL")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let prepared = startup(tune::prepare(std::path::Path::new(&workspace)))
        .await
        .unwrap()
        .with_launcher_env(vec![(
            "LOOM_TEST_REPLAY_ENDPOINT".to_owned(),
            listener.local_addr().unwrap().to_string(),
        )]);
    let clock = ManualClock::new();
    let request = Request::Propose(ProposeRequest {
        surface: Surface::Skill,
        level: loom_tune::checker::Level::Run,
        targets: vec!["loom-scope-discipline".to_owned()],
        dry_run: false,
        seed: Some(7),
    });
    let mut operation = Box::pin(prepared.execute_with_clock(request, &clock));
    let response = if hang {
        let peers = startup(async {
            tokio::select! {
                response = &mut operation => panic!("replay finished before lifecycle readiness: {response:?}"),
                peers = ready(&listener) => peers,
            }
        }).await;
        if !release {
            clock.advance(Duration::from_secs(wall));
        }
        // Pipe closure is the cancellation boundary. Proposal commits/publication remain
        // part of the startup/whole-fixture watchdog, not the five-second OS cleanup check.
        let (response, ()) = startup(async {
            tokio::join!(
                operation,
                cleanup(async {
                    for peer in peers {
                        peer.finish(!release).await;
                    }
                })
            )
        })
        .await;
        response.unwrap()
    } else {
        startup(operation).await.unwrap()
    };
    assert!(matches!(response, Response::Proposal(_)), "{response:?}");
}

async fn startup<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(180), future)
        .await
        .expect("replay fixture exceeded its OS startup watchdog")
}

async fn cleanup<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("replay fixture exceeded its cancellation cleanup deadline")
}

async fn ready(listener: &TcpListener) -> Vec<Peer> {
    let mut peers = Vec::new();
    for _ in 0..2 {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut pid = String::new();
        reader.read_line(&mut pid).await.unwrap();
        peers.push(Peer {
            stream: reader.into_inner(),
            pid: nix::unistd::Pid::from_raw(pid.trim().parse().unwrap()),
            live: true,
        });
    }
    peers
}

struct Peer {
    stream: TcpStream,
    pid: nix::unistd::Pid,
    live: bool,
}

fn closed_connection(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
    )
}

impl Peer {
    async fn finish(mut self, stopped: bool) {
        match self.stream.write_all(b"go\n").await {
            Ok(()) => {}
            Err(error) if closed_connection(&error) => {}
            Err(error) => panic!("release replay fixture: {error}"),
        }
        let mut response = Vec::new();
        match self.stream.read_to_end(&mut response).await {
            Ok(_) => {}
            Err(error) if closed_connection(&error) => {}
            Err(error) => panic!("observe replay fixture exit: {error}"),
        }
        self.live = false;
        if stopped {
            assert!(
                response.is_empty(),
                "replay process survived budget expiry: {response:?}"
            );
        } else {
            assert_eq!(response, b"escaped\n", "live fixture must write on release");
        }
    }
}

impl Drop for Peer {
    #[expect(
        clippy::print_stderr,
        reason = "fixture destructor cannot return OS cleanup failures"
    )]
    fn drop(&mut self) {
        if self.live {
            match nix::sys::signal::kill(self.pid, nix::sys::signal::Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(error) => eprintln!("replay peer cleanup failed: {error}"),
            }
        }
    }
}
