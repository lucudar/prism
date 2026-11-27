//! Уборка при удалении Prism. Её запускает деинсталлятор (src-tauri/windows/hooks.nsh):
//! `prism.exe --cleanup`, а с галочкой «Удалить данные приложения» ещё и `--purge`.
//! Prism меняет в Windows несколько вещей, и раньше они оставались после удаления:
//! блок адресов нейросетей в hosts, задание автозапуска в планировщике и служба
//! драйвера WinDivert. При обновлении деинсталлятор уборку не запускает.

use std::time::Duration;

use crate::engine::{self, ZapretPaths};
use crate::util::{run_quiet, Log};

pub fn run(purge: bool) {
    // Работающий Prism закрываем первым: его сторож снова запустил бы winws.exe.
    let me = format!("PID ne {}", std::process::id());
    let name = std::env::current_exe().ok().and_then(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()));
    if let Some(name) = name {
        run_quiet("taskkill", &["/F", "/IM", &name, "/FI", &me]);
    }
    // Прокси Telegram иначе держал бы свой .exe, и деинсталлятор не смог бы его удалить.
    run_quiet("taskkill", &["/F", "/IM", "tg-ws-proxy.exe"]);
    std::thread::sleep(Duration::from_millis(800));

    let data = crate::secure::data_dir(&Log::default()).ok();
    if let Some(data) = &data {
        // Установленная и портативная копии хранят движок в разных папках.
        for root in [data.join("zapret"), data.join("portable").join("zapret")] {
            engine::kill_stray(&ZapretPaths { root });
        }
    }
    // Службу драйвера убираем, как «Remove Services» в service.bat Flowseal.
    // Если работает другой zapret, драйвер нужен ему, и его не трогаем.
    if engine::winws_processes().is_empty() {
        for svc in ["WinDivert", "WinDivert14"] {
            run_quiet("sc", &["stop", svc]);
            run_quiet("sc", &["delete", svc]);
        }
    }
    let _ = crate::hosts::disable();
    run_quiet("schtasks", &["/Delete", "/F", "/TN", crate::TASK_NAME]);
    if purge {
        if let Some(data) = data {
            let _ = std::fs::remove_dir_all(data);
        }
    }
}
