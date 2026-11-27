//! Уборка при удалении Prism. Её запускает деинсталлятор (src-tauri/windows/hooks.nsh):
//! `prism.exe --cleanup`, а с галочкой «Удалить данные приложения» ещё и `--purge`.
//! Prism меняет в Windows несколько вещей, и раньше они оставались после удаления:
//! блок адресов нейросетей в hosts, задание автозапуска в планировщике и служба
//! драйвера WinDivert. При обновлении деинсталлятор уборку не запускает.
//! Если что-то убрать не вышло, Prism показывает, что осталось, и предлагает повторить.
//! С `--quiet` (тихое удаление) окон нет, о неудаче говорит только код выхода.

use std::path::Path;
use std::time::Duration;

use crate::engine::{self, ZapretPaths};
use crate::util::{run_quiet, Log};

/// Процессы с таким именем файла: номер и полный путь (пустой, если не узнать).
/// Как engine::winws_processes, только для любого имени.
fn processes(name: &str) -> Vec<(u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let mut out = Vec::new();
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return out;
    }
    let mut e: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut more = unsafe { Process32FirstW(snap, &mut e) } != 0;
    while more {
        let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
        if String::from_utf16_lossy(&e.szExeFile[..len]).eq_ignore_ascii_case(name) {
            let mut path = String::new();
            let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, e.th32ProcessID) };
            if !h.is_null() {
                let mut buf = [0u16; 1024];
                let mut n = buf.len() as u32;
                if unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut n) } != 0 {
                    path = String::from_utf16_lossy(&buf[..n as usize]);
                }
                unsafe { CloseHandle(h) };
            }
            out.push((e.th32ProcessID, path));
        }
        more = unsafe { Process32NextW(snap, &mut e) } != 0;
    }
    unsafe { CloseHandle(snap) };
    out
}

/// Пути сравниваются без учёта регистра и префикса \\?\.
fn same_file(a: &str, b: &Path) -> bool {
    let plain = |s: &str| s.strip_prefix(r"\\?\").unwrap_or(s).to_lowercase();
    !a.is_empty() && plain(a) == plain(&b.display().to_string())
}

/// Закрыть процессы, запущенные именно из этого файла. Одноимённые программы
/// из других папок (портативный Prism, свой tg-ws-proxy) не трогаем.
fn kill_exe(exe: &Path) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    let Some(name) = exe.file_name().map(|n| n.to_string_lossy().into_owned()) else { return };
    let me = std::process::id();
    for (pid, path) in processes(&name) {
        if pid == me || !same_file(&path, exe) {
            continue;
        }
        let h = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
        if !h.is_null() {
            unsafe {
                TerminateProcess(h, 1);
                WaitForSingleObject(h, 3000);
                CloseHandle(h);
            }
        }
    }
}

/// Вернуть Windows прежний вид. Возвращает то, что убрать не удалось.
fn undo(purge: Option<&Path>) -> Vec<String> {
    let mut failed = Vec::new();
    if let Err(e) = crate::hosts::disable() {
        failed.push(e);
    }
    // Задание удаляем, только если оно есть: без задания schtasks /Delete тоже ошибка.
    if run_quiet("schtasks", &["/Query", "/TN", crate::TASK_NAME])
        && !run_quiet("schtasks", &["/Delete", "/F", "/TN", crate::TASK_NAME])
    {
        failed.push("Не удалось удалить задание автозапуска «Prism» из планировщика.".to_string());
    }
    if let Some(dir) = purge.filter(|d| d.exists()) {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            failed.push(format!("Не удалось удалить папку {}: {e}", dir.display()));
        }
    }
    failed
}

/// Спросить, повторить ли уборку. Окно поверх деинсталлятора, он ждёт ответа.
fn ask_retry(failed: &[String]) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDRETRY, MB_ICONWARNING, MB_RETRYCANCEL, MB_SETFOREGROUND, MB_TOPMOST,
    };
    let w = |s: &str| std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let list: Vec<String> = failed.iter().map(|f| format!("• {f}")).collect();
    let body = format!(
        "Prism не смог убрать за собой всё:\n\n{}\n\nНажмите «Повторить», чтобы попробовать ещё раз (например, после отключения антивируса), или «Отмена», чтобы продолжить удаление.",
        list.join("\n")
    );
    let style = MB_RETRYCANCEL | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST;
    unsafe { MessageBoxW(std::ptr::null_mut(), w(&body).as_ptr(), w("Удаление Prism").as_ptr(), style) == IDRETRY }
}

/// Убрать всё, что Prism менял в Windows. Возвращает, удалось ли убрать всё.
pub fn run(purge: bool, quiet: bool) -> bool {
    if let Ok(exe) = std::env::current_exe() {
        // Работающий Prism закрываем первым: его сторож снова запустил бы winws.exe.
        kill_exe(&exe);
        // Прокси Telegram этой копии держал бы свой .exe, и деинсталлятор не смог бы его удалить.
        if let Some(dir) = exe.parent() {
            kill_exe(&dir.join("resources").join("tgproxy").join("tg-ws-proxy.exe"));
        }
    }
    std::thread::sleep(Duration::from_millis(500));

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
    let purge_dir = if purge { data.as_deref() } else { None };
    let mut failed = undo(purge_dir);
    while !failed.is_empty() && !quiet && ask_retry(&failed) {
        failed = undo(purge_dir);
    }
    failed.is_empty()
}
