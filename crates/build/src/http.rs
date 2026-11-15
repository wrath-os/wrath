//! Downloads over HTTP in wrath's own code, so a machine that fetches needs no curl.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// One agent for the command, so requests to the same host reuse its connections.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(600)))
            .build()
            .into()
    })
}

/// Downloads `url` into `file`, or says there's nothing there (404). Any other answer is an
/// error.
pub fn download(url: &str, file: &Path) -> Result<bool, String> {
    download_with(url, None, file)
}

/// Downloads `url` into `file` as [`download`] does, sending `token` as a bearer token. The token
/// goes only to `url` itself: a redirect is followed without it.
pub fn download_with(url: &str, token: Option<&str>, file: &Path) -> Result<bool, String> {
    let mut request = agent().get(url);
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
}
