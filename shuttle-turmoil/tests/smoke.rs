use std::sync::Once;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream};

static INIT: Once = Once::new();

fn init_tracing() {
    INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .init();
    });
}

async fn connect(addr: &str) -> TcpStream {
    loop {
        match TcpStream::connect(addr).await {
            Ok(s) => return s,
            Err(_) => shuttle::future::yield_now().await,
        }
    }
}

/// Basic TCP echo — verifies the turmoil-net + Shuttle integration works.
#[test]
fn tcp_echo() {
    init_tracing();
    shuttle_turmoil::check(
        || {
            let mut sim = shuttle_turmoil::Sim::new();
            sim.host("server", || async {
                let listener = TcpListener::bind("0.0.0.0:9000").await.unwrap();
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 5];
                sock.read_exact(&mut buf).await.unwrap();
                sock.write_all(&buf).await.unwrap();
            });
            sim.client("client", async {
                let mut c = connect("server:9000").await;
                c.write_all(b"hello").await.unwrap();
                let mut buf = [0u8; 5];
                c.read_exact(&mut buf).await.unwrap();
                assert_eq!(&buf, b"hello");
            });
            sim.run();
        },
        1000,
    );
}

/// Read-modify-write over the network: each client reads the server's
/// counter (1 byte), increments it, writes it back on a new connection.
/// The server rejects any write that isn't strictly greater — if both
/// clients read before either writes, one write won't be. Shuttle finds
/// this interleaving.
#[test]
#[should_panic(expected = "stale write")]
fn read_modify_write_race() {
    init_tracing();
    shuttle_turmoil::check(
        || {
            let server = || async {
                let listener = TcpListener::bind("0.0.0.0:7000").await.unwrap();
                let mut stored: u8 = 0;
                // 2 reads + 2 writes = 4 requests
                for _ in 0..4 {
                    let (mut sock, _) = listener.accept().await.unwrap();
                    let mut buf = [0u8; 1];
                    sock.read_exact(&mut buf).await.unwrap();
                    match buf[0] {
                        b'R' => sock.write_all(&[stored]).await.unwrap(),
                        val => {
                            assert!(
                                val > stored,
                                "stale write: got {val}, stored is {stored}"
                            );
                            stored = val;
                        }
                    }
                }
            };

            let client = || async {
                // Read
                let mut c = connect("server:7000").await;
                c.write_all(&[b'R']).await.unwrap();
                let mut buf = [0u8; 1];
                c.read_exact(&mut buf).await.unwrap();

                // Modify + Write
                let mut c = connect("server:7000").await;
                c.write_all(&[buf[0] + 1]).await.unwrap();
            };

            let mut sim = shuttle_turmoil::Sim::new();
            sim.host("server", server);
            sim.client("client_a", client());
            sim.client("client_b", client());
            sim.run();
        },
        1000,
    );
}
