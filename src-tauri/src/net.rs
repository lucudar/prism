//! Сеть, через которую идёт интернет. Провайдеры режут по-разному: дома может
//! работать одна стратегия, а через телефон другая. Prism запоминает стратегию
//! для каждой сети и сам переключает её, когда сеть меняется.

use crate::util::hidden;

/// Имя сети так, как его показывает Windows: имя Wi-Fi или «Сеть 2» для кабеля.
/// Вывод в UTF-8, иначе русские имена пришли бы в кодировке консоли.
pub fn current() -> Option<String> {
    let script = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-NetConnectionProfile | Where-Object { $_.IPv4Connectivity -eq 'Internet' -or $_.IPv6Connectivity -eq 'Internet' } | Select-Object -First 1 -ExpandProperty Name";
    let out = hidden("powershell").args(["-NoProfile", "-NonInteractive", "-Command", script]).output().ok()?;
    clean(&String::from_utf8_lossy(&out.stdout))
}

/// Первая непустая строка без метки порядка байтов, не длиннее 64 символов.
fn clean(out: &str) -> Option<String> {
    let line = out.lines().map(|l| l.trim().trim_start_matches('\u{feff}').trim()).find(|l| !l.is_empty())?;
    Some(line.chars().take(64).collect())
}

/// Ждать, пока у компьютера поменяются IP-адреса: подключились к другой сети,
/// переподключили Wi-Fi, вставили кабель. Блокирует поток до перемены,
/// процессор при этом не тратится.
pub fn wait_change() -> bool {
    use windows_sys::Win32::NetworkManagement::IpHelper::NotifyAddrChange;
    unsafe { NotifyAddrChange(std::ptr::null_mut(), std::ptr::null()) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_name() {
        assert_eq!(clean("\r\nHome-WiFi\r\n").as_deref(), Some("Home-WiFi"));
        assert_eq!(clean("\u{feff}Сеть 2\r\n").as_deref(), Some("Сеть 2"));
        assert_eq!(clean("  \r\n"), None);
        assert_eq!(clean(&"x".repeat(100)).map(|s| s.chars().count()), Some(64));
    }
}
