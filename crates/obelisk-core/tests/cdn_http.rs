//! What a CDN's socket sees of a `CdnClient` and what the client makes of the
//! socket: the one test of its network side, since everything else about the
//! request runs against a CDN in memory.
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use obelisk_core::transfer::{BodySize, CancelToken, CdnClient, CdnError, set_speed_limit};
use reqwest::Url;

/// How a loopback CDN answers its next connection.
enum Answer {
    Body(Vec<u8>),
    Status(u16),
    /// Promises `body`, sends it in `pieces` and waits `every` after each.
    Slowly {
        body: Vec<u8>,
        pieces: usize,
        every: Duration,
    },
    /// Promises `promised` bytes, sends `sent` and closes the connection.
    Truncated {
        promised: usize,
        sent: Vec<u8>,
    },
    /// Closes the connection without a word.
    Hangup,
}

/// A CDN that answers one connection for each of `answers`, in order, and
/// records the head of each request.
struct Server {
    url: Url,
    heads: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start(answers: Vec<Answer>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!("http://{}/chunk", listener.local_addr().unwrap())).unwrap();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&heads);
        thread::spawn(move || {
            for answer in answers {
                let (mut socket, _) = listener.accept().unwrap();
                let mut head = Vec::new();
                let mut buffer = [0; 1024];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    match socket.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => head.extend_from_slice(&buffer[..count]),
                    }
                }
                recorded
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&head).into_owned());
                let _ = respond(&mut socket, answer);
            }
        });
        Self { url, heads }
    }

    fn head(&self, index: usize) -> String {
        self.heads.lock().unwrap()[index].to_lowercase()
    }
}

fn respond(socket: &mut std::net::TcpStream, answer: Answer) -> std::io::Result<()> {
    match answer {
        Answer::Body(body) => {
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            socket.write_all(&body)
        }
        Answer::Status(status) => write!(
            socket,
            "HTTP/1.1 {status} Answer\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
        Answer::Slowly {
            body,
            pieces,
            every,
        } => {
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            for piece in body.chunks(body.len().div_ceil(pieces)) {
                socket.write_all(piece)?;
                socket.flush()?;
                thread::sleep(every);
            }
            Ok(())
        }
        Answer::Truncated { promised, sent } => {
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {promised}\r\nConnection: close\r\n\r\n"
            )?;
            socket.write_all(&sent)
        }
        Answer::Hangup => Ok(()),
    }
}

async fn get(client: &CdnClient, url: &Url, size: BodySize) -> Result<Vec<u8>, CdnError> {
    client
        .get(url.clone(), size, &CancelToken::new(), |_| {})
        .await
}

// One test: the speed limit is the process's, and tests of one binary share it.
#[tokio::test]
async fn a_request_is_sent_and_read_through_a_socket_the_way_every_storefront_expects() {
    let body = b"a chunk of a game".to_vec();
    let server = Server::start(vec![
        Answer::Body(body.clone()),
        Answer::Status(503),
        Answer::Hangup,
        Answer::Truncated {
            promised: 100,
            sent: vec![7; 40],
        },
    ]);
    let client = CdnClient::new(reqwest::Client::new());
    let size = BodySize::AtMost(1 << 20);

    assert_eq!(get(&client, &server.url, size).await, Ok(body));
    let head = server.head(0);
    assert!(head.starts_with("get /chunk "), "{head}");
    assert!(head.contains("accept-encoding: identity"), "{head}");
    assert!(
        !head.contains("authorization"),
        "a CDN request carries no credential: {head}"
    );
    assert_eq!(
        get(&client, &server.url, size).await,
        Err(CdnError::Http(503))
    );
    assert_eq!(
        get(&client, &server.url, size).await,
        Err(CdnError::Network),
        "a connection closed without an answer"
    );
    assert_eq!(
        get(&client, &server.url, size).await,
        Err(CdnError::Network),
        "a body cut short"
    );
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        Url::parse(&format!("http://{}/chunk", listener.local_addr().unwrap())).unwrap()
    };
    assert_eq!(
        get(&client, &closed, size).await,
        Err(CdnError::Network),
        "a refused connection"
    );

    // The client's own total timeout, such as the storefronts' 45 s or 90 s,
    // would end a chunk that is slow but flowing. The request replaces it.
    let slow = || Answer::Slowly {
        body: vec![7; 100],
        pieces: 5,
        every: Duration::from_millis(100),
    };
    let server = Server::start(vec![slow(), slow()]);
    let short = reqwest::Client::builder()
        .timeout(Duration::from_millis(200))
        .build()
        .unwrap();
    let plain = async { short.get(server.url.clone()).send().await?.bytes().await }.await;
    assert!(
        plain.is_err_and(|error| error.is_timeout()),
        "the control: this client ends a body that takes 500 ms"
    );
    assert_eq!(
        get(&CdnClient::new(short), &server.url, BodySize::Exactly(100)).await,
        Ok(vec![7; 100])
    );

    // The speed limit of the player holds a request that went through a
    // socket, for as long as it is set: 700 kB at 625 kB/s take over a second.
    let answers = || Answer::Body(vec![0; 700_000]);
    let server = Server::start(vec![answers(), answers(), answers()]);
    let before = timed(&client, &server.url, size).await;
    set_speed_limit(625_000);
    let held = timed(&client, &server.url, size).await;
    set_speed_limit(0);
    let after = timed(&client, &server.url, size).await;
    assert!(held.1 >= Duration::from_secs(1), "{:?}", held.1);
    // Unlimited requests are not held: each is quicker than the limited one,
    // whatever the load of the machine.
    assert!(before.1 < held.1, "{:?} against {:?}", before.1, held.1);
    assert!(after.1 < held.1, "{:?} against {:?}", after.1, held.1);
    for (body, _) in [before, held, after] {
        assert_eq!(body.map(|body| body.len()), Ok(700_000));
    }
}

/// A request's result and how long it took.
async fn timed(
    client: &CdnClient,
    url: &Url,
    size: BodySize,
) -> (Result<Vec<u8>, CdnError>, Duration) {
    let started = Instant::now();
    let body = get(client, url, size).await;
    (body, started.elapsed())
}
