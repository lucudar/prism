//! Сеть, через которую идёт интернет. Провайдеры режут по-разному: дома может
//! работать одна стратегия, а через телефон другая. Prism запоминает стратегию
//! для каждой сети и сам переключает её, когда сеть меняется.

use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use crate::util::hidden;

/// Имя сети так, как его показывает Windows: имя Wi-Fi или «Сеть 2» для кабеля.
/// Берём сеть интерфейса с маршрутом по умолчанию (сначала IPv4, потом IPv6), а не ту,
/// которую Windows считает подключённой к интернету: эта проверка (NCSI) ошибается,
/// когда провайдер режет сайты Microsoft. Пока Windows опознаёт сеть, профиля у
/// интерфейса нет, и имя пустое. Вывод в UTF-8, иначе русские имена пришли бы
/// в кодировке консоли.
pub fn current() -> Option<String> {
    let script = concat!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8; ",
        "$r = Get-NetRoute -DestinationPrefix '0.0.0.0/0','::/0' -ErrorAction SilentlyContinue | ",
        "Sort-Object { $_.AddressFamily -ne 'IPv4' }, { $_.RouteMetric + $_.InterfaceMetric } | Select-Object -First 1; ",
        "if ($r) { Get-NetConnectionProfile -InterfaceIndex $r.InterfaceIndex -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Name }"
    );
    let out = hidden("powershell").args(["-NoProfile", "-NonInteractive", "-Command", script]).output().ok()?;
    clean(&String::from_utf8_lossy(&out.stdout))
}

/// Первая непустая строка без метки порядка байтов, не длиннее 64 символов.
fn clean(out: &str) -> Option<String> {
    let line = out.lines().map(|l| l.trim().trim_start_matches('\u{feff}').trim()).find(|l| !l.is_empty())?;
    Some(line.chars().take(64).collect())
}

/// Сигналы о переменах в сети: приходят, когда у компьютера меняются IPv4-адреса
/// (подключились к другой сети, переподключили Wi-Fi, вставили кабель). Отдельный поток
/// спит в Windows до перемены и процессор не тратит. Перемены только в IPv6 сюда
/// не доходят, поэтому получатель ещё и сверяется с сетью по таймеру.
pub fn changes() -> Receiver<()> {
    let (tx, rx) = channel();
    std::thread::spawn(move || loop {
        if !wait_change() {
            std::thread::sleep(Duration::from_secs(60));
            continue;
        }
        if tx.send(()).is_err() {
            return;
        }
    });
    rx
}

fn wait_change() -> bool {
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
