//! Over-the-air updates through a small HTTP server.
//!
//! Every request except `GET /` needs `Authorization: Bearer <ota_password>`.
//!
//! - `GET /`: JSON with the device's name, version and state.
//! - `POST /update/firmware`: an app image (`espflash save-image` output),
//!   written to the inactive app slot. The device reboots into it, and rolls
//!   back if the new firmware does not come up.
//! - `POST /update/config`: a configuration written by `fennec`.
//! - `POST /update/model`: a `.fkw` wake word model.
//!
//! The device restarts after a successful update.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use embedded_svc::http::Method;
use embedded_svc::io::Write;
use esp_idf_svc::http::server::{Configuration, EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::ota::EspOta;
use fennec_core::config::Config;
use fennec_core::model::{self, Model};

use crate::storage::{self, Partition};

/// Set while an update is being received, so the main loop can show it.
pub static UPDATING: AtomicBool = AtomicBool::new(false);

const CHUNK: usize = 4096;

pub struct Status {
    pub name: String,
    pub wake_word: String,
}

/// Starts the server. Updates are refused when `password` is empty.
pub fn start(password: String, status: Status) -> anyhow::Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&Configuration {
        stack_size: 10 * 1024,
        max_uri_handlers: 8,
        ..Default::default()
    })?;
    let password: &'static str = password.leak();
    let status: &'static Status = Box::leak(Box::new(status));

    server.fn_handler("/", Method::Get, move |req| -> anyhow::Result<()> {
        let free = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() };
        let uptime = unsafe { esp_idf_svc::sys::esp_timer_get_time() } / 1_000_000;
        let body = format!(
            "{{\"name\":{:?},\"version\":{:?},\"wake_word\":{:?},\"uptime_s\":{uptime},\"free_heap\":{free},\"updates\":{}}}\n",
            status.name,
            env!("CARGO_PKG_VERSION"),
            status.wake_word,
            !password.is_empty(),
        );
        req.into_response(200, None, &[("Content-Type", "application/json")])?
            .write_all(body.as_bytes())?;
        Ok(())
    })?;

    for (uri, kind) in [
        ("/update/firmware", Kind::Firmware),
        ("/update/config", Kind::Config),
        ("/update/model", Kind::Model),
    ] {
        server.fn_handler(uri, Method::Post, move |req| -> anyhow::Result<()> {
            handle(req, kind, password)
        })?;
    }
    Ok(server)
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Firmware,
    Config,
    Model,
}

fn respond(
    req: Request<&mut EspHttpConnection<'_>>,
    status: u16,
    message: &str,
) -> anyhow::Result<()> {
    req.into_status_response(status)?
        .write_all(format!("{message}\n").as_bytes())?;
    Ok(())
}

/// Compares without leaking where the first difference is.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn handle(
    mut req: Request<&mut EspHttpConnection<'_>>,
    kind: Kind,
    password: &str,
) -> anyhow::Result<()> {
    if password.is_empty() {
        return respond(
            req,
            403,
            "updates are disabled: set ota.password in the configuration",
        );
    }
    let authorized = req
        .header("Authorization")
        .and_then(|h| h.strip_prefix("Bearer "))
        .is_some_and(|token| same(token, password));
    if !authorized {
        return respond(req, 401, "wrong or missing password");
    }
    let Some(length) = req
        .header("Content-Length")
        .and_then(|l| l.parse::<usize>().ok())
    else {
        return respond(req, 411, "Content-Length is required");
    };

    if UPDATING.swap(true, Ordering::SeqCst) {
        return respond(req, 409, "another update is in progress");
    }
    log::info!("receiving {kind:?} update ({length} bytes)");
    let result = receive(&mut req, kind, length);
    match result {
        Ok(()) => {
            log::info!("{kind:?} update written, restarting");
            respond(req, 200, "updated, restarting")?;
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(500));
                storage::restart();
            });
            Ok(())
        }
        Err(e) => {
            UPDATING.store(false, Ordering::SeqCst);
            log::error!("{kind:?} update failed: {e:#}");
            respond(req, 400, &format!("update failed: {e:#}"))
        }
    }
}

fn read_chunks(
    req: &mut Request<&mut EspHttpConnection<'_>>,
    length: usize,
    mut sink: impl FnMut(&[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; CHUNK];
    let mut remaining = length;
    while remaining > 0 {
        let want = remaining.min(CHUNK);
        let n = req
            .read(&mut buf[..want])
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        if n == 0 {
            bail!("connection closed with {remaining} bytes missing");
        }
        sink(&buf[..n])?;
        remaining -= n;
    }
    Ok(())
}

fn receive(
    req: &mut Request<&mut EspHttpConnection<'_>>,
    kind: Kind,
    length: usize,
) -> anyhow::Result<()> {
    match kind {
        Kind::Firmware => {
            let mut ota = EspOta::new()?;
            let mut update = ota.initiate_update()?;
            let mut first = true;
            let result = read_chunks(req, length, |chunk| {
                // ESP32 app images start with 0xE9.
                if first && chunk.first() != Some(&0xE9) {
                    bail!("not an ESP32 app image; send the output of `espflash save-image`");
                }
                first = false;
                Ok(update.write(chunk)?)
            });
            match result {
                Ok(()) => update.complete().context("the image is invalid")?,
                Err(e) => {
                    update.abort().ok();
                    return Err(e);
                }
            }
        }
        Kind::Config => {
            if length > 4096 {
                bail!("configuration too large");
            }
            let mut bytes = Vec::with_capacity(length);
            read_chunks(req, length, |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(())
            })?;
            Config::decode(&bytes)?;
            let mut writer = Partition::find(storage::CONFIG)?.writer();
            writer.write(&bytes)?;
            writer.finish()?;
        }
        Kind::Model => {
            let partition = Partition::find(storage::MODEL)?;
            if length > partition.size() {
                bail!(
                    "model too large for the {} byte partition",
                    partition.size()
                );
            }
            // Check the header before erasing the current model.
            let mut header = Vec::new();
            let mut writer = None;
            read_chunks(req, length, |chunk| {
                if writer.is_none() {
                    header.extend_from_slice(chunk);
                    if header.len() < 4 && header.len() < length {
                        return Ok(());
                    }
                    if header.get(..4) != Some(&model::MAGIC) {
                        bail!("not a Fennec wake word model (.fkw)");
                    }
                    let mut w = partition.writer();
                    w.write(&header)?;
                    writer = Some(w);
                    return Ok(());
                }
                writer.as_mut().unwrap().write(chunk)
            })?;
            writer.context("empty model")?.finish()?;
            // The mapping sees what is now in flash.
            Model::from_bytes(partition.map()?).context("the written model is invalid")?;
        }
    }
    Ok(())
}
