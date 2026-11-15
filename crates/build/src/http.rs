//! Downloads over HTTP in wrath's own code, so a machine that fetches needs no curl.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// One agent for the command, so requests to the same host reuse its connections. Each request
/// gets a time limit of its own, as long as what it fetches needs.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build()
            .into()
    })
}

/// How long a download of a source of unknown size may take.
const SOURCE_TIME: Duration = Duration::from_secs(1800);

/// Downloads `url` into `file`, or says there's nothing there (404). Any other answer is an
/// error.
pub fn download(url: &str, file: &Path) -> Result<bool, String> {
    download_with(url, None, file)
}

/// Downloads `url` into `file` as [`download`] does, sending `token` as a bearer token. The token
/// goes only to `url` itself: a redirect is followed without it.
pub fn download_with(url: &str, token: Option<&str>, file: &Path) -> Result<bool, String> {
    let mut request = agent()
        .get(url)
        .config()
        .timeout_global(Some(SOURCE_TIME))
        .build();
    if let Some(token) = token {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    let mut response = request.call().map_err(|e| format!("{url}: {e}"))?;
    match response.status().as_u16() {
        200 => {}
        404 => return Ok(false),
        code => return Err(format!("{url}: {code}")),
    }
    let mut written = fs::File::create(file).map_err(|e| format!("{}: {e}", file.display()))?;
    std::io::copy(&mut response.body_mut().as_reader(), &mut written)
        .map_err(|e| format!("{url}: {e}"))?;
    Ok(true)
}

/// What fetching an object from a cache found.
#[derive(Debug, PartialEq, Eq)]
pub enum Object {
    /// It's in the file now.
    Got,
    /// The cache doesn't have it: 404, or 403 as a bucket says it to whoever may not list it.
    Missing,
}

/// Downloads `url` into `file`, at most `limit` bytes, as an object in a cache. Answers that may
/// pass, a lost connection or a server's error, are tried again twice, a little later each time;
/// what's still wrong after that is an error. The time it may take grows with `limit`.
pub fn object(url: &str, file: &Path, limit: u64) -> Result<Object, String> {
    let mut last = String::new();
    for attempt in 0..3u32 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(500 * u64::from(attempt)));
        }
        match object_once(url, file, limit) {
            Ok(found) => return Ok(found),
            Err(Failure::Final(message)) => return Err(message),
            Err(Failure::Passing(message)) => last = message,
        }
    }
    Err(last)
}

enum Failure {
    /// Worth trying again: the connection, or the server's own error.
    Passing(String),
    Final(String),
}

fn object_once(url: &str, file: &Path, limit: u64) -> Result<Object, Failure> {
    // A minute, and as long again as `limit` takes at 100 KB/s.
    let time = Duration::from_secs(60 + limit / 100_000);
    let request = agent().get(url).config().timeout_global(Some(time)).build();
    let mut response = request
        .call()
        .map_err(|e| Failure::Passing(format!("{url}: {e}")))?;
    match response.status().as_u16() {
        200 => {}
        403 | 404 => return Ok(Object::Missing),
        code @ 500..=599 => return Err(Failure::Passing(format!("{url}: {code}"))),
        code => return Err(Failure::Final(format!("{url}: {code}"))),
    }
    let failed = |e: std::io::Error| Failure::Final(format!("{}: {e}", file.display()));
    let mut written = fs::File::create(file).map_err(failed)?;
    let body = response.body_mut().as_reader().take(limit + 1);
    let copied = std::io::copy(&mut { body }, &mut written)
        .map_err(|e| Failure::Passing(format!("{url}: {e}")))?;
    if copied > limit {
        return Err(Failure::Final(format!(
            "{url} is larger than the {limit} bytes expected"
        )));
    }
    Ok(Object::Got)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// Answers one request with `answer`, given the request's Authorization header, or "none".
    fn serve(answer: impl Fn(&str) -> String + Send + 'static) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut authorization = "none".to_owned();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("authorization")
                {
                    authorization = value.trim().to_owned();
                }
            }
            (&stream)
                .write_all(answer(&authorization).as_bytes())
                .unwrap();
        });
        port
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn a_token_goes_to_the_url_asked_for_and_never_on_through_a_redirect() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("got");

        let port = serve(ok);
        let url = format!("http://127.0.0.1:{port}/");
        assert!(download_with(&url, Some("secret"), &file).unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), "Bearer secret");

        let elsewhere = serve(ok);
        let port = serve(move |_| {
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{elsewhere}/\r\n\
                 Content-Length: 0\r\n\r\n"
            )
        });
        let url = format!("http://127.0.0.1:{port}/");
        assert!(download_with(&url, Some("secret"), &file).unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), "none");
    }

    #[test]
    fn a_cache_object_is_missing_on_403_as_on_404_and_refused_when_larger_than_expected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("got");
        let status =
            |code: u16| move |_: &str| format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\n\r\n");
        for code in [403, 404] {
            let url = format!("http://127.0.0.1:{}/", serve(status(code)));
            assert_eq!(object(&url, &file, 100), Ok(Object::Missing), "{code}");
        }
        let url = format!("http://127.0.0.1:{}/", serve(|_| ok("twelve bytes")));
        assert_eq!(object(&url, &file, 12), Ok(Object::Got));
        let url = format!("http://127.0.0.1:{}/", serve(|_| ok("twelve bytes")));
        let error = object(&url, &file, 11).unwrap_err();
        assert!(error.contains("larger than the 11 bytes"), "{error}");
        let url = format!("http://127.0.0.1:{}/", serve(status(401)));
        assert!(object(&url, &file, 100).unwrap_err().ends_with(": 401"));
    }
}
