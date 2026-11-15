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
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_global(Some(Duration::from_secs(600)))
            .build()
            .into()
    })
}

/// Downloads `url` into `file`, or says there's nothing there (404). Any other answer is an
/// error.
pub fn download(url: &str, file: &Path) -> Result<bool, String> {
    let mut response = agent().get(url).call().map_err(|e| format!("{url}: {e}"))?;
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
