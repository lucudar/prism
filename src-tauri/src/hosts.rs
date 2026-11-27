//! Доступ к нейросетям: адреса из публичного списка ImMALWARE/dns.malw.link
//! записываются в файл hosts Windows. Берём только разделы с нейросетями.

use std::fs;
use std::path::PathBuf;

use crate::util::run_quiet;

const URL: &str = "https://raw.githubusercontent.com/ImMALWARE/dns.malw.link/master/hosts";
const START: &str = "# >>> Prism: нейросети (не редактируйте этот блок)";
const END: &str = "# <<< Prism";
const SECTIONS: &[&str] = &["OpenAI", "Microsoft", "GitHub Copilot", "ElevenLabs", "Grok", "DeepL", "Google", "Claude"];

pub fn hosts_path() -> PathBuf {
    crate::secure::system_dir().join("drivers").join("etc").join("hosts")
}

/// Оставить из списка только нужные разделы и строки «IP домен».
pub fn filter_list(raw: &str) -> String {
    let mut out = Vec::new();
    let mut take = false;
    for line in raw.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix("# ") {
            if !t.starts_with("###") {
                take = SECTIONS.iter().any(|s| s.eq_ignore_ascii_case(name.trim()));
                if take {
                    out.push(format!("# {}", name.trim()));
                }
            }
            continue;
        }
        if !take || t.is_empty() || t.starts_with('#') || t.starts_with("0.0.0.0") {
            continue;
        }
        let mut it = t.split_whitespace();
        if let (Some(ip), Some(host), None) = (it.next(), it.next(), it.next()) {
            if ip.parse::<std::net::IpAddr>().is_ok() && host.contains('.') {
                out.push(format!("{ip} {host}"));
            }
        }
    }
    out.join("\r\n")
}

/// Строка, которую Prism мог записать в свой блок: заголовок раздела или «IP домен».
fn block_line(line: &str) -> bool {
    let t = line.trim();
    if let Some(name) = t.strip_prefix("# ") {
        return SECTIONS.iter().any(|s| s.eq_ignore_ascii_case(name.trim()));
    }
    let mut it = t.split_whitespace();
    matches!((it.next(), it.next(), it.next()), (Some(ip), Some(host), None) if ip.parse::<std::net::IpAddr>().is_ok() && host.contains('.'))
}

fn strip_block(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != START {
            out.push(lines[i]);
            i += 1;
            continue;
        }
        match lines[i + 1..].iter().position(|l| l.trim() == END) {
            Some(n) => i += n + 2,
            // Конец блока стёрли вручную: убираем только строки, похожие на наши,
            // и останавливаемся на первой чужой, чтобы не стереть остаток файла.
            None => {
                i += 1;
                while i < lines.len() && block_line(lines[i]) {
                    i += 1;
                }
            }
        }
    }
    while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    out.join("\r\n")
}

fn write_hosts(body: &str) -> Result<(), String> {
    let p = hosts_path();
    if let Ok(meta) = fs::metadata(&p) {
        let mut perm = meta.permissions();
        if perm.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = fs::set_permissions(&p, perm);
        }
    }
    crate::util::write_atomic(&p, body).map_err(|e| {
        format!("Не удалось изменить файл hosts: {e}. Возможно, его блокирует антивирус.")
    })?;
    run_quiet("ipconfig", &["/flushdns"]);
    Ok(())
}

pub async fn download() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let text = client
        .get(URL)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Не удалось скачать список адресов: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let list = filter_list(&text);
    if list.lines().filter(|l| !l.starts_with('#')).count() < 5 {
        return Err("Список адресов пришёл пустым, попробуйте позже".into());
    }
    Ok(list)
}

pub fn enable(list: &str) -> Result<(), String> {
    let current = fs::read_to_string(hosts_path()).unwrap_or_default();
    let body = format!("{}\r\n\r\n{START}\r\n{list}\r\n{END}\r\n", strip_block(&current));
    write_hosts(&body)
}

pub fn disable() -> Result<(), String> {
    let current = fs::read_to_string(hosts_path()).unwrap_or_default();
    if !current.contains(START) {
        return Ok(());
    }
    write_hosts(&format!("{}\r\n", strip_block(&current)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_sections() {
        let raw = "### dns.malw.link: hosts file\n# OpenAI\n1.2.3.4 chatgpt.com\n# Spotify\n5.6.7.8 spotify.com\n# Блокировка\n0.0.0.0 ads.com\n### end\n";
        let f = filter_list(raw);
        assert!(f.contains("1.2.3.4 chatgpt.com"));
        assert!(!f.contains("spotify"));
        assert!(!f.contains("ads.com"));
    }

    #[test]
    fn strips_block() {
        let t = format!("127.0.0.1 localhost\r\n\r\n{START}\r\n1.1.1.1 a.com\r\n{END}\r\n");
        assert_eq!(strip_block(&t), "127.0.0.1 localhost");
        let mid = format!("1.1.1.1 a.com\r\n{START}\r\n# OpenAI\r\n2.2.2.2 chatgpt.com\r\n{END}\r\n3.3.3.3 b.com\r\n");
        assert_eq!(strip_block(&mid), "1.1.1.1 a.com\r\n3.3.3.3 b.com");
    }

    #[test]
    fn keeps_rest_without_end_marker() {
        let t = format!("127.0.0.1 localhost\r\n{START}\r\n# OpenAI\r\n2.2.2.2 chatgpt.com\r\n\r\n# Моё\r\n10.0.0.5 nas.local\r\n");
        assert_eq!(strip_block(&t), "127.0.0.1 localhost\r\n\r\n# Моё\r\n10.0.0.5 nas.local");
    }
}
