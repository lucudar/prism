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

/// Запустить системную команду и дождаться её, игнорируя ошибки.
pub fn run_quiet(program: &str, args: &[&str]) -> bool {
    hidden(program)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Сколько последних строк хранит журнал. Подбор перезапускает движок два десятка
/// раз, и при 400 строках начало подбора терялось.
const MAX_LINES: usize = 3000;
/// Сколько строк диагностики можно закрепить в начале журнала.
const MAX_PINNED: usize = 40;

#[derive(Default)]
struct Lines {
    /// Диагностика: всегда в начале журнала, её не вытесняет вывод движка.
    pinned: Vec<String>,
    recent: VecDeque<String>,
}

/// Общий журнал: диагностика и последние строки вывода движка, прокси и самой программы.
#[derive(Clone, Default)]
pub struct Log(Arc<Mutex<Lines>>);

impl Log {
    pub fn push(&self, line: impl Into<String>) {
        let mut l = self.0.lock().unwrap();
        l.recent.push_back(line.into());
        while l.recent.len() > MAX_LINES {
            l.recent.pop_front();
        }
    }

    /// Закрепить строку в начале журнала. Одинаковые строки не повторяются.
    pub fn pin(&self, line: impl Into<String>) {
        let line = line.into();
        let mut l = self.0.lock().unwrap();
        if l.pinned.len() < MAX_PINNED && !l.pinned.contains(&line) {
            l.pinned.push(line);
        }
    }

    pub fn text(&self) -> String {
        let l = self.0.lock().unwrap();
        let mut out: Vec<&str> = l.pinned.iter().map(String::as_str).collect();
        if !out.is_empty() && !l.recent.is_empty() {
            out.push("[prism] ---");
        }
        out.extend(l.recent.iter().map(String::as_str));
        out.join("\n")
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
    fn keeps_pinned_lines() {
        let log = Log::default();
        log.pin("[prism] сборка Windows: 19045");
        log.pin("[prism] сборка Windows: 19045");
        for i in 0..MAX_LINES + 10 {
            log.push(format!("line {i}"));
        }
        let text = log.text();
        assert!(text.starts_with("[prism] сборка Windows: 19045\n[prism] ---\nline 10\n"));
        assert_eq!(text.matches("Windows").count(), 1);
        assert!(text.ends_with(&format!("line {}", MAX_LINES + 9)));
    }
}
