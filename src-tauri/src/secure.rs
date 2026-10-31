//! Защищённая папка %ProgramData%\Prism. Prism работает с правами администратора,
//! поэтому всё, что он запускает или записывает, лежит там, где обычная программа
//! ничего не подменит: менять папку могут только администраторы и SYSTEM.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SetSecurityInfo, SDDL_REVISION_1,
    SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, IsWellKnownSid, WinBuiltinAdministratorsSid, WinLocalSystemSid, ACL,
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, GetFileInformationByHandle, GetFinalPathNameByHandleW, MoveFileExW,
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
    WRITE_DAC,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, FOLDERID_System, SHGetKnownFolderPath,
};

use crate::util::Log;

/// Владелец — администраторы, доступ есть только у SYSTEM и администраторов.
/// Разрешения от ProgramData не наследуются (P), вложенные папки и файлы получают эти же.
const SDDL: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    let mut p = null_mut();
    let hr = unsafe { SHGetKnownFolderPath(id, 0, null_mut(), &mut p) };
    let out = if hr == 0 && !p.is_null() {
        let len = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
        Some(PathBuf::from(OsString::from_wide(unsafe { std::slice::from_raw_parts(p, len) })))
    } else {
        None
    };
    unsafe { CoTaskMemFree(p as _) };
    out
}

/// Папка System32. Берётся у Windows, а не из переменной окружения,
/// которую может поменять сам пользователь.
pub fn system_dir() -> PathBuf {
    known_folder(&FOLDERID_System).unwrap_or_else(|| PathBuf::from("C:\\Windows\\System32"))
}

/// Лежит ли программа в Program Files. Туда без прав администратора не пишут,
/// а портативную копию в папке пользователя может подменить любая программа.
pub fn in_program_files(exe: &Path) -> bool {
    let Ok(exe) = exe.canonicalize() else { return false };
    [FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86]
        .iter()
        .filter_map(known_folder)
        .filter_map(|p| p.canonicalize().ok())
        .any(|p| exe.starts_with(p))
}

struct Sd(PSECURITY_DESCRIPTOR);

impl Drop for Sd {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

impl Sd {
    fn ours() -> Result<Sd, String> {
        let mut p = null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(wide(SDDL).as_ptr(), SDDL_REVISION_1, &mut p, null_mut())
        };
        if ok == 0 {
            return Err(format!("Не удалось подготовить права папки (код {})", unsafe { GetLastError() }));
        }
        Ok(Sd(p))
    }

    fn dacl(&self) -> *mut ACL {
        let (mut present, mut defaulted, mut dacl) = (0, 0, null_mut());
        unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) };
        dacl
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

enum State {
    Missing,
    Trusted,
    /// Папку создал не администратор, это ссылка или не папка вовсе.
    Foreign,
}

fn inspect(dir: &Path, sd: &Sd) -> State {
    let h = unsafe {
        CreateFileW(
            wide(dir).as_ptr(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        let e = unsafe { GetLastError() };
        return if e == ERROR_FILE_NOT_FOUND || e == ERROR_PATH_NOT_FOUND { State::Missing } else { State::Foreign };
    }
    let h = Handle(h);
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(h.0, &mut info) } == 0 {
        return State::Foreign;
    }
    let a = info.dwFileAttributes;
    if a & FILE_ATTRIBUTE_DIRECTORY == 0 || a & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return State::Foreign;
    }
    // Назначить владельцем администраторов обычный пользователь не может,
    // поэтому такой владелец значит, что папку создал администратор.
    let (mut owner, mut psd) = (null_mut(), null_mut());
    let r = unsafe {
        GetSecurityInfo(h.0, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, &mut owner, null_mut(), null_mut(), null_mut(), &mut psd)
    };
    if r != 0 {
        return State::Foreign;
    }
    let _owner_sd = Sd(psd);
    let trusted = unsafe { IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0 || IsWellKnownSid(owner, WinLocalSystemSid) != 0 };
    if !trusted {
        return State::Foreign;
    }
    // Права могли ослабить вручную, поэтому каждый раз ставим их заново.
    let r = unsafe {
        SetSecurityInfo(
            h.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            sd.dacl(),
            null(),
        )
    };
    if r != 0 {
        return State::Foreign;
    }
    State::Trusted
}

/// Папка %ProgramData%\Prism с правами только для администраторов и SYSTEM.
/// Если такую папку заранее создала другая программа, она откладывается в сторону.
pub fn data_dir(log: &Log) -> Result<PathBuf, String> {
    let base = known_folder(&FOLDERID_ProgramData).ok_or("Не найдена папка ProgramData")?;
    prepare(&base, log)
}

fn prepare(base: &Path, log: &Log) -> Result<PathBuf, String> {
    let dir = base.join("Prism");
    let sd = Sd::ours()?;
    for _ in 0..3 {
        match inspect(&dir, &sd) {
            State::Trusted => return Ok(dir),
            State::Missing => {
                let sa = SECURITY_ATTRIBUTES {
                    nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: sd.0,
                    bInheritHandle: 0,
                };
                // Результат проверит inspect на следующем круге.
                unsafe { CreateDirectoryW(wide(&dir).as_ptr(), &sa) };
            }
            State::Foreign => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let aside = base.join(format!("Prism.untrusted-{ts}"));
                if unsafe { MoveFileExW(wide(&dir).as_ptr(), wide(&aside).as_ptr(), 0) } == 0 {
                    return Err(format!("Папку {} создала другая программа, и её не удалось убрать", dir.display()));
                }
                log.push(format!("[prism] папку {} создала другая программа, она перенесена в {}", dir.display(), aside.display()));
            }
        }
    }
    Err(format!("Не удалось подготовить защищённую папку {}", dir.display()))
}

/// Прочитать файл из папки, куда может писать пользователь (настройки прошлых версий).
/// Ссылки не открываются: если путь куда-то перенаправлен, файл не читается.
/// Иначе через подставленную ссылку можно было бы прочитать чужой файл с правами администратора.
pub fn read_user_file(path: &Path, max: u64) -> Option<String> {
    let f = std::fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(path).ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() || meta.len() > max || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return None;
    }
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetFinalPathNameByHandleW(f.as_raw_handle(), buf.as_mut_ptr(), buf.len() as u32, 0) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    let real = OsString::from_wide(&buf[..n]).to_string_lossy().to_lowercase();
    let want = format!("\\\\?\\{}", path.display()).to_lowercase();
    if real != want {
        return None;
    }
    std::io::read_to_string(f).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_file_but_not_through_junction() {
        // Полный путь без префикса \\?\ и коротких имён вида RUNNER~1.
        let tmp = std::env::temp_dir().canonicalize().unwrap();
        let tmp = PathBuf::from(tmp.to_string_lossy().trim_start_matches(r"\\?\"));
        let dir = tmp.join("prism-secure-test");
        let _ = std::fs::remove_dir_all(&dir);
        let (real, link) = (dir.join("real"), dir.join("link"));
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.txt"), "site.ru").unwrap();
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&real).output().unwrap();
        assert!(made.status.success());
        assert_eq!(read_user_file(&real.join("a.txt"), 100).as_deref(), Some("site.ru"));
        assert_eq!(read_user_file(&real.join("a.txt"), 3), None);
        assert_eq!(read_user_file(&link.join("a.txt"), 100), None);
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn run(cmd: &str, args: &[&str]) -> String {
        let o = std::process::Command::new(cmd).args(args).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// Папка, которую создал обычный пользователь. У администратора с повышенными
    /// правами владельцем новых папок становятся «Администраторы», поэтому владельца
    /// меняем на самого пользователя, как было бы без повышения.
    fn plant(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("winws.exe"), "fake").unwrap();
        let user = format!("{}\\{}", std::env::var("USERDOMAIN").unwrap(), std::env::var("USERNAME").unwrap());
        let ok = std::process::Command::new("icacls").arg(dir).args(["/setowner", &user]).output().unwrap().status.success();
        assert!(ok);
    }

    fn sddl(p: &Path) -> String {
        run("powershell", &["-NoProfile", "-Command", &format!("(Get-Acl -LiteralPath '{}').Sddl", p.display())])
    }

    fn aside(base: &Path) -> usize {
        std::fs::read_dir(base).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("Prism.untrusted-")).count()
    }

    /// Папку, созданную пользователем, нельзя принять за свою. Без прав администратора
    /// новую с владельцем «Администраторы» создать не получится, с правами — получится.
    #[test]
    fn rejects_user_folder() {
        let base = std::env::temp_dir().join("prism-prepare-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        plant(&base.join("Prism"));
        let r = prepare(&base, &Log::default());
        let elevated = std::process::Command::new("net").arg("session").output().map(|o| o.status.success()).unwrap_or(false);
        assert_eq!(aside(&base), 1);
        if !elevated {
            assert!(r.is_err());
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        let dir = r.unwrap();
        assert!(!dir.join("winws.exe").exists());

        // Владелец — администраторы, доступ только у SYSTEM и администраторов, без наследования.
        let s = sddl(&dir);
        println!("папка: {s}");
        assert!(s.starts_with("O:BA"), "{s}");
        assert!(s.contains("D:P"), "{s}");
        assert_eq!(s.matches("(A;").count(), 2, "{s}");
        assert!(s.contains(";;;SY)") && s.contains(";;;BA)"), "{s}");
        // Вложенные файлы получают те же права.
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let f = sddl(&dir.join("a.txt"));
        println!("файл: {f}");
        assert_eq!(f.matches("(A;").count(), 2, "{f}");
        assert!(f.contains(";;;SY)") && f.contains(";;;BA)"), "{f}");

        // Повторный запуск принимает свою папку и ничего не переносит.
        std::fs::write(dir.join("keep.txt"), "x").unwrap();
        assert_eq!(prepare(&base, &Log::default()).unwrap(), dir);
        assert!(dir.join("keep.txt").exists());
        assert_eq!(aside(&base), 1);

        // Ослабленные вручную права возвращаются.
        assert!(std::process::Command::new("icacls").arg(&dir).args(["/grant", "*S-1-5-32-545:(OI)(CI)F"]).output().unwrap().status.success());
        assert_eq!(sddl(&dir).matches("(A;").count(), 3);
        prepare(&base, &Log::default()).unwrap();
        assert_eq!(sddl(&dir).matches("(A;").count(), 2, "{}", sddl(&dir));

        // Ссылка на месте папки тоже откладывается в сторону.
        std::fs::remove_dir_all(&dir).unwrap();
        let target = base.join("target");
        plant(&target);
        assert!(std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&dir).arg(&target).output().unwrap().status.success());
        let again = prepare(&base, &Log::default()).unwrap();
        assert!(!again.join("winws.exe").exists());
        assert!(target.join("winws.exe").exists());
        assert_eq!(aside(&base), 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn finds_system_folders() {
        assert!(system_dir().join("drivers").join("etc").exists());
        assert!(!in_program_files(&std::env::temp_dir()));
    }
}
