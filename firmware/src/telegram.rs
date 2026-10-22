//! Uploads recordings to a Telegram chat with the Bot API: as voice notes
//! (`sendVoice`) with the `opus` feature, otherwise as WAV documents
//! (`sendDocument`). Only built with the `telegram` feature.

use anyhow::{Context, bail};
use embedded_svc::http::client::Client;
use embedded_svc::io::Write;
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};

use fennec_core::config::Config;

use crate::Recording;

const BOUNDARY: &str = "fennec-7f3a9c2e41b84d6b";

/// How a recording format is uploaded.
pub struct Upload {
    /// The Bot API method.
    pub method: &'static str,
    /// The multipart field holding the file.
    pub field: &'static str,
    pub content_type: &'static str,
    pub extension: &'static str,
}

/// Sends `recording` with `caption`, as a file named `<name>.<extension>`.
pub fn send(
    config: &Config,
    recording: &Recording,
    upload: &Upload,
    caption: &str,
    name: &str,
) -> anyhow::Result<()> {
    let mut fields = String::new();
    let mut field = |name: &str, value: &str| {
        fields.push_str(&format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        ));
    };
    field("chat_id", &config.chat_id);
    if let Some(thread) = config.message_thread_id {
        field("message_thread_id", &thread.to_string());
    }
    field("caption", caption);
    if upload.method == "sendVoice" {
        field("duration", &(recording.seconds().round() as u32).to_string());
    }
    fields.push_str(&format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{}\"; \
         filename=\"{name}.{}\"\r\nContent-Type: {}\r\n\r\n",
        upload.field, upload.extension, upload.content_type
    ));
    let closing = format!("\r\n--{BOUNDARY}--\r\n");
    let header = recording.header();
    let length = fields.len() + header.len() + recording.bytes() + closing.len();

    let connection = EspHttpConnection::new(&Configuration {
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        timeout: Some(core::time::Duration::from_secs(30)),
        ..Default::default()
    })?;
    let mut client = Client::wrap(connection);
    let url = format!(
        "https://api.telegram.org/bot{}/{}",
        config.bot_token, upload.method
    );
    let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
    let length_header = length.to_string();
    let headers = [
        ("Content-Type", content_type.as_str()),
        ("Content-Length", length_header.as_str()),
    ];

    let mut request = client
        .post(&url, &headers)
        .context("cannot connect to api.telegram.org")?;
    request.write_all(fields.as_bytes())?;
    request.write_all(&header)?;
    for chunk in recording.chunks() {
        request.write_all(chunk)?;
    }
    request.write_all(closing.as_bytes())?;
    request.flush()?;
    let mut response = request.submit()?;

    let status = response.status();
    let mut body = [0u8; 512];
    let mut read = 0;
    while read < body.len() {
        match response.read(&mut body[read..])? {
            0 => break,
            n => read += n,
        }
    }
    let body = String::from_utf8_lossy(&body[..read]);
    if status != 200 || !body.contains("\"ok\":true") {
        bail!("Telegram answered {status}: {body}");
    }
    Ok(())
}
