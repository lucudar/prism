//! Локальный MTProto-прокси для Telegram (Flowseal/tg-ws-proxy, собирается из исходников в CI).

use std::path::Path;
use std::process::{Child, Stdio};

use crate::util::{hidden, Log};

pub const HOST: &str = "127.0.0.1";

pub fn new_secret() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Секрет передаётся прокси в командной строке, поэтому принимаем только 32 hex-символа.
pub fn valid_secret(s: &str) -> bool {
    s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn link(port: u16, secret: &str) -> String {
    format!("tg://proxy?server={HOST}&port={port}&secret=dd{secret}")
}

#[derive(Default)]
pub struct Proxy {
    child: Option<Child>,
}

impl Proxy {
    pub fn start(&mut self, exe: &Path, port: u16, secret: &str, log: &Log) -> Result<(), String> {
        self.stop();
        if !exe.exists() {
            return Err("Не найден tg-ws-proxy.exe. Переустановите Prism.".into());
        }
        log.push(format!("[prism] запуск прокси Telegram на {HOST}:{port}"));
        let mut child = hidden(exe)
            .args(["--host", HOST, "--port", &port.to_string(), "--secret", secret])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| format!("Не удалось запустить прокси: {e}"))?;
        if let Some(o) = child.stdout.take() {
            log.pipe("tg", o);
        }
        if let Some(e) = child.stderr.take() {
            log.pipe("tg", e);
        }
        self.child = Some(child);
        Ok(())
    }

    pub fn alive(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_secret() {
        assert!(valid_secret(&new_secret()));
        assert!(!valid_secret("0123456789abcdef0123456789abcde"));
        assert!(!valid_secret("0123456789abcdef0123456789abcde --x"));
        assert!(!valid_secret("0123456789abcdef0123456789abcdeg"));
    }
}
