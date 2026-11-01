use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::Command;
use std::sync::{Arc, Mutex};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Флаг Windows: запускать процесс без окна консоли.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Команда, которая запускается без мигающего окна консоли.
pub fn hidden(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Записать файл целиком или не менять вовсе: сначала во временный файл рядом,
/// потом подменить. Если Prism закроют или выключат свет посреди записи,
/// останется старый файл, а не обрезанный. ReplaceFileW сохраняет права
/// и атрибуты старого файла, это важно для hosts.
pub fn write_atomic(path: &std::path::Path, body: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, REPLACEFILE_WRITE_THROUGH,
    };

    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".prism-tmp");
    let tmp = path.with_file_name(name);
    let wide = |p: &std::path::Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_ref())?;
        f.sync_all()?;
        drop(f);
        let (to, from) = (wide(path), wide(&tmp));
        let ok = if path.exists() {
            unsafe { ReplaceFileW(to.as_ptr(), from.as_ptr(), std::ptr::null(), REPLACEFILE_WRITE_THROUGH, std::ptr::null(), std::ptr::null()) }
        } else {
            unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Запустить системную команду и дождаться её, игнорируя ошибки.
pub fn run_quiet(program: &str, args: &[&str]) -> bool {
    hidden(program)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Общий журнал: последние строки вывода движка, прокси и самой программы.
#[derive(Clone, Default)]
pub struct Log(Arc<Mutex<VecDeque<String>>>);

impl Log {
    pub fn push(&self, line: impl Into<String>) {
        let mut q = self.0.lock().unwrap();
        q.push_back(line.into());
        while q.len() > 400 {
            q.pop_front();
        }
    }

    pub fn text(&self) -> String {
        self.0.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// Читать поток процесса построчно в журнал (в отдельном потоке).
    pub fn pipe<R: Read + Send + 'static>(&self, tag: &'static str, reader: R) {
        let log = self.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let line = line.trim_end();
                if !line.is_empty() {
                    log.push(format!("[{tag}] {line}"));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_file_whole() {
        let dir = std::env::temp_dir().join("prism-atomic-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.txt");
        write_atomic(&p, "один").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "один");
        write_atomic(&p, "два").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "два");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
