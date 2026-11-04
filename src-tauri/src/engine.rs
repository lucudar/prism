//! Управление движком zapret (winws.exe из сборки Flowseal).
//! Стратегии берутся из оригинальных .bat-файлов Flowseal без изменений:
//! мы только достаём из них аргументы winws.exe и подставляем пути.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

use crate::util::{hidden, run_quiet, Log};

#[derive(Clone, Serialize)]
pub struct Strategy {
    pub id: String,
    pub name: String,
    pub desc: String,
}

#[derive(Clone)]
pub struct ZapretPaths {
    pub root: PathBuf,
}

impl ZapretPaths {
    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }
    pub fn lists(&self) -> PathBuf {
        self.root.join("lists")
    }
    pub fn strategies(&self) -> PathBuf {
        self.root.join("strategies")
    }
    pub fn user_list(&self) -> PathBuf {
        self.lists().join("list-general-user.txt")
    }
}

fn describe(name: &str) -> &'static str {
    let n = name.to_uppercase();
    if n == "ОСНОВНАЯ" {
        "Рекомендуемая, начните с неё"
    } else if n.contains("FAKE TLS") {
        "Поддельное TLS-приветствие, часто помогает на мобильном интернете"
    } else if n.contains("SIMPLE FAKE") {
        "Простые фейковые пакеты"
    } else if n.contains("EXP") {
        "Экспериментальная"
    } else {
        "Альтернативная разрезка пакетов"
    }
}

fn natural_key(s: &str) -> (String, u32) {
    let digits: String = s.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<String>().chars().rev().collect();
    let prefix = s[..s.len() - digits.len()].to_string();
    (prefix, digits.parse().unwrap_or(0))
}

pub fn list_strategies(paths: &ZapretPaths) -> Vec<Strategy> {
    let mut out = Vec::new();
    if let Ok(dir) = fs::read_dir(paths.strategies()) {
        for e in dir.flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            if !file.to_lowercase().ends_with(".bat") || !file.to_lowercase().starts_with("general") {
                continue;
            }
            let stem = file[..file.len() - 4].to_string();
            let name = match (stem.find('('), stem.rfind(')')) {
                (Some(a), Some(b)) if b > a => stem[a + 1..b].trim().to_string(),
                _ => "Основная".to_string(),
            };
            out.push(Strategy { desc: describe(&name).into(), id: file, name });
        }
    }
    out.sort_by(|a, b| {
        let ka = if a.name == "Основная" { (String::new(), 0) } else { natural_key(&a.name) };
        let kb = if b.name == "Основная" { (String::new(), 0) } else { natural_key(&b.name) };
        ka.cmp(&kb)
    });
    out
}

/// Разбить командную строку на аргументы с учётом кавычек.
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            // Как в cmd.exe: ^ вне кавычек экранирует следующий символ (например, ^!).
            '^' if !quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    has = true;
                }
            }
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// Достать аргументы winws.exe из .bat-файла Flowseal.
pub fn parse_bat(text: &str, paths: &ZapretPaths, games: bool) -> Result<Vec<String>, String> {
    let marker = "winws.exe\"";
    let idx = text.find(marker).ok_or("В файле стратегии не найден запуск winws.exe")?;
    let mut parts = Vec::new();
    for line in text[idx + marker.len()..].lines() {
        let t = line.trim_end();
        if let Some(stripped) = t.strip_suffix('^') {
            parts.push(stripped.to_string());
        } else {
            parts.push(t.to_string());
            break;
        }
    }
    let joined = parts.join(" ");
    let bin = format!("{}\\", paths.bin().display());
    let lists = format!("{}\\", paths.lists().display());
    let (tcp, udp) = if games { (GAMES_TCP, GAMES_UDP) } else { (GAMES_OFF, GAMES_OFF) };
    let joined = joined
        .replace("%BIN%", &bin)
        .replace("%LISTS%", &lists)
        .replace("%GameFilterTCP%", tcp)
        .replace("%GameFilterUDP%", udp)
        .replace("%GameFilter%", tcp);
    if joined.contains('%') {
        return Err("В стратегии осталась неизвестная переменная".into());
    }
    let args = youtube_quic(tokenize(&joined), paths);
    Ok(if games { games_ipset(args, paths) } else { args })
}

/// Как в service.bat Flowseal: без игрового режима ставится заглушка-порт 12.
const GAMES_OFF: &str = "12";
/// Порты игрового режима для TCP, как «TCP and UDP» в service.bat Flowseal.
pub const GAMES_TCP: &str = "1024-65535";
/// Порты игрового режима для UDP. Порт 7770 пропускаем: по нему игры на AWS GameLift
/// (например, Dead by Daylight) меряют пинг до регионов. Если менять эти пакеты,
/// игра выбирает дальний регион и пинг вырастает в разы (Flowseal, issue #17165).
pub const GAMES_UDP: &str = "1024-7769,7771-65535";

/// Разбить аргументы на правила winws: каждое заканчивается на --new.
fn split_groups(args: Vec<String>) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = vec![Vec::new()];
    for a in args {
        let end = a == "--new";
        if let Some(g) = groups.last_mut() {
            g.push(a);
        }
        if end {
            groups.push(Vec::new());
        }
    }
    groups
}

/// Вставить аргумент в конец правила, перед --new.
fn push_arg(g: &mut Vec<String>, arg: String) {
    let at = if g.last().map(|s| s == "--new").unwrap_or(false) { g.len() - 1 } else { g.len() };
    g.insert(at, arg);
}

/// YouTube по QUIC (UDP 443). В стратегиях Flowseal домены YouTube из list-google.txt
/// есть только в правиле для TCP 443, а QUIC к googlevideo.com обходится лишь правилом
/// по IP. С заглушкой в ipset-all.txt оно ничего не ловит, и видео в браузере зависело
/// от того, успеет ли он откатиться на TCP. Добавляем list-google.txt в правило QUIC
/// по доменам: способ обхода в нём тот же, что и в правиле QUIC по IP.
fn youtube_quic(args: Vec<String>, paths: &ZapretPaths) -> Vec<String> {
    let lists = paths.lists();
    if !lists.join("list-google.txt").exists() {
        return args;
    }
    let google = format!("--hostlist={}\\list-google.txt", lists.display());
    let mut groups = split_groups(args);
    let quic = groups.iter_mut().find(|g| {
        g.iter().any(|a| a == "--filter-udp=443" || a == "--filter-l7=quic")
            && g.iter().any(|a| a.starts_with("--hostlist=") && a.ends_with("list-general.txt"))
            && !g.iter().any(|a| a.starts_with("--ipset="))
    });
    if let Some(g) = quic {
        if !g.iter().any(|a| *a == google) {
            match g.iter().rposition(|a| a.starts_with("--hostlist=")) {
                Some(i) => g.insert(i + 1, google),
                None => push_arg(g, google),
            }
        }
    }
    groups.concat()
}

/// Полный список подсетей от Flowseal (в его service.bat это «IPSet: loaded»).
pub const GAMES_IPSET: &str = "ipset-all.txt.backup";
/// Сети издателей игр, лаунчеров и античитов (Riot, Valve, Blizzard, Roblox, EA, Epic и др.).
/// Игры на них работают без обхода, и Prism их пакеты не меняет, чтобы не ломать вход в игры.
pub const GAMES_EXCLUDE: &str = "ipset-exclude-games.txt";
const GAMES_EXCLUDE_BODY: &str = include_str!("../lists/ipset-exclude-games.txt");

/// Игровой режим. Правила для игровых портов работают только для серверов из полного
/// списка подсетей и не трогают сети издателей игр и античитов. Правила для сайтов
/// остаются как были, поэтому игровой режим их не затрагивает.
fn games_ipset(args: Vec<String>, paths: &ZapretPaths) -> Vec<String> {
    let lists = paths.lists();
    let stub = format!("--ipset={}\\ipset-all.txt", lists.display());
    let full = format!("--ipset={}\\{GAMES_IPSET}", lists.display());
    let exclude = format!("--ipset-exclude={}\\{GAMES_EXCLUDE}", lists.display());
    let (tcp, udp) = (format!("--filter-tcp={GAMES_TCP}"), format!("--filter-udp={GAMES_UDP}"));
    let mut out = Vec::new();
    for mut g in split_groups(args) {
        let game = g.iter().any(|a| *a == tcp || *a == udp);
        if game && g.iter().any(|a| *a == stub) {
            for a in g.iter_mut() {
                if *a == stub {
                    *a = full.clone();
                }
            }
            push_arg(&mut g, exclude.clone());
        }
        out.extend(g);
    }
    out
}

/// Файлы игрового режима: список исключений обновляется вместе с Prism.
/// Если полного списка подсетей нет, ставим заглушку, чтобы движок всё равно запустился.
pub fn ensure_game_lists(paths: &ZapretPaths, log: &Log) {
    let l = paths.lists();
    let _ = fs::write(l.join(GAMES_EXCLUDE), GAMES_EXCLUDE_BODY);
    let full = l.join(GAMES_IPSET);
    if !full.exists() {
        log.push("[prism] нет полного списка подсетей, игровой режим работает только для Discord");
        let _ = fs::write(full, format!("{IPSET_STUB}\r\n"));
    }
}

/// Заглушка Flowseal: адрес из документационной сети, которого нет в интернете.
const IPSET_STUB: &str = "203.0.113.113/32";

/// Общие правила по IP работают с заглушкой, как «IPSet: none» в service.bat Flowseal
/// (так Flowseal поставляется). Полный список ipset-all.txt.backup покрывает почти весь
/// AWS, Cloudflare, Google, Akamai и Microsoft, а на них живут серверы игр, лаунчеров
/// и античитов: Prism 0.10 подставлял его сюда, и с обходом переставали работать игры.
/// Полный список остаётся в ipset-all.txt.backup для игрового режима.
fn ensure_ipset_stub(l: &Path) {
    let file = l.join("ipset-all.txt");
    let stub = fs::read_to_string(&file).map(|t| t.lines().any(|x| x.trim() == IPSET_STUB)).unwrap_or(false);
    if stub {
        return;
    }
    let backup = l.join(GAMES_IPSET);
    if file.exists() && !backup.exists() {
        let _ = fs::rename(&file, &backup);
    }
    let _ = fs::write(&file, format!("{IPSET_STUB}\r\n"));
}

/// Создать пользовательские списки так же, как это делает service.bat.
pub fn ensure_user_lists(paths: &ZapretPaths) {
    let l = paths.lists();
    ensure_ipset_stub(&l);
    let files = [
        ("ipset-exclude-user.txt", "203.0.113.113/32\r\n"),
        ("list-general-user.txt", "# Never leave this file empty\r\ndomain.example.abc\r\n"),
        ("list-exclude-user.txt", "domain.example.abc\r\n"),
    ];
    for (name, body) in files {
        let p = l.join(name);
        if !p.exists() {
            let _ = fs::write(p, body);
        }
    }
}

/// Включить метки времени TCP, как service.bat Flowseal перед каждым запуском.
/// Половина стратегий подделывает пакеты с --dpi-desync-fooling=ts: сервер должен
/// выбросить подделку из-за старой метки времени. В Windows метки по умолчанию
/// выключены, и тогда подделка доходит до сервера как настоящие данные и рвёт
/// соединение. Команда меняет одну системную настройку, выполняем её раз за сеанс.
pub fn enable_tcp_timestamps(log: &Log) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    if run_quiet("netsh", &["interface", "tcp", "set", "global", "timestamps=enabled"]) {
        log.push("[prism] метки времени TCP включены (нужны стратегиям с fooling=ts)");
    } else {
        log.push("[prism] не удалось включить метки времени TCP, стратегии с fooling=ts могут не работать");
    }
}

/// Вывод системной команды или пустая строка.
fn output(program: &str, args: &[&str]) -> String {
    hidden(program)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Что из найденного в списке служб (`sc query`) мешает zapret. Проверки те же,
/// что в пункте «Run Diagnostics» service.bat Flowseal.
fn conflicts(services: &str) -> Vec<&'static str> {
    let s = services.to_lowercase();
    let mut out = Vec::new();
    if s.contains("killer") {
        out.push("Killer Network Service (Intel Killer)");
    }
    if s.lines().any(|l| l.contains("intel") && l.contains("connectivity") && l.contains("network")) {
        out.push("Intel Connectivity Network Service");
    }
    if s.contains("tracsrvwrapper") || s.contains("epwd") {
        out.push("Check Point");
    }
    if s.contains("smartbyte") {
        out.push("SmartByte");
    }
    if s.contains("goodbyedpi") {
        out.push("GoodbyeDPI");
    }
    out
}

/// Службы VPN из списка `sc query`. Названия полей sc переводит на язык Windows,
/// поэтому смотрим на структуру: у каждой службы сначала строка с именем, затем
/// строка с описанием, обе без отступа.
fn vpn_services(services: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut name = String::new();
    let mut prev_top = false;
    for line in services.lines() {
        let value = line.split_once(':').map(|(_, v)| v.trim()).filter(|_| !line.starts_with(char::is_whitespace));
        if let Some(value) = value {
            if !prev_top {
                name = value.to_string();
            }
            let low = value.to_lowercase();
            if (low.contains("vpn") || low.contains("happservice")) && !name.is_empty() && !out.contains(&name) {
                out.push(name.clone());
            }
        }
        prev_top = value.is_some();
    }
    out
}

/// Есть ли в файле hosts свои адреса для YouTube: с ними видео часто не грузится.
fn hosts_youtube(text: &str) -> bool {
    text.lines().map(str::trim).filter(|l| !l.starts_with('#')).any(|l| {
        let l = l.to_lowercase();
        l.contains("youtube.com") || l.contains("youtu.be") || l.contains("googlevideo.com")
    })
}

/// Проверить, что мешает обходу. Только пишет в журнал и ничего не меняет.
pub fn diagnose(log: &Log) {
    let services = output("sc", &["query"]);
    for name in conflicts(&services) {
        log.push(format!("[prism] найдено: {name}. Эта программа мешает zapret: если обход не работает, отключите её"));
    }
    if output("tasklist", &["/FI", "IMAGENAME eq AdguardSvc.exe"]).to_lowercase().contains("adguardsvc.exe") {
        log.push("[prism] запущен Adguard. Он может мешать Discord и YouTube: если они не открываются, выключите его");
    }
    let vpn = vpn_services(&services);
    if !vpn.is_empty() {
        log.push(format!("[prism] найдены службы VPN: {}. Если обход не работает, выключите VPN", vpn.join(", ")));
    }
    if hosts_youtube(&fs::read_to_string(crate::hosts::hosts_path()).unwrap_or_default()) {
        log.push("[prism] в файле hosts есть адреса YouTube. Из-за них видео может не грузиться, уберите эти строки");
    }
}

pub fn kill_stray() {
    run_quiet("taskkill", &["/F", "/IM", "winws.exe"]);
}

/// Выгрузить драйвер WinDivert, когда обход выключен. Пока драйвер загружен,
/// некоторые античиты не дают зайти в игру.
pub fn unload_driver() {
    run_quiet("sc", &["stop", "WinDivert"]);
    run_quiet("sc", &["stop", "WinDivert14"]);
}

#[derive(Default)]
pub struct Engine {
    child: Option<Child>,
    /// Когда обход включился. Перезапуск со сменой стратегии время не сбрасывает.
    since: Option<std::time::SystemTime>,
}

impl Engine {
    pub fn start(&mut self, paths: &ZapretPaths, strategy: &str, games: bool, log: &Log) -> Result<(), String> {
        let keep = if self.alive() { self.since } else { None };
        self.stop();
        kill_stray();
        ensure_user_lists(paths);
        if games {
            ensure_game_lists(paths, log);
        }
        enable_tcp_timestamps(log);
        let bat = paths.strategies().join(strategy);
        let text = fs::read_to_string(&bat).map_err(|e| format!("Не удалось прочитать стратегию: {e}"))?;
        let args = parse_bat(&text, paths, games)?;
        let exe = paths.bin().join("winws.exe");
        if !exe.exists() {
            return Err("Не найден winws.exe. Переустановите Prism.".into());
        }
        log.push(format!("[prism] запуск стратегии {strategy}, игровой режим: {games}"));
        let mut child = hidden(&exe)
            .args(&args)
            .current_dir(paths.bin())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| format!("Не удалось запустить winws.exe: {e}"))?;
        if let Some(out) = child.stdout.take() {
            log.pipe("winws", out);
        }
        if let Some(err) = child.stderr.take() {
            log.pipe("winws", err);
        }
        self.child = Some(child);
        self.since = Some(keep.unwrap_or_else(std::time::SystemTime::now));
        Ok(())
    }

    /// Работает ли процесс движка прямо сейчас.
    pub fn alive(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Время включения в миллисекундах с 1970 года или 0, если обход не работает.
    pub fn since_ms(&mut self) -> u64 {
        if !self.alive() {
            return 0;
        }
        self.since
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.since = None;
    }
}

/// Скопировать файлы zapret из папки программы в рабочую папку пользователя,
/// если версия изменилась. Пользовательские списки сохраняются.
pub fn install(resource: &Path, target: &Path, log: &Log) -> Result<(), String> {
    let src_ver = fs::read_to_string(resource.join("version.txt")).unwrap_or_default();
    let dst_ver = fs::read_to_string(target.join("version.txt")).unwrap_or_default();
    let has_engine = target.join("bin").join("winws.exe").exists();
    // Не откатываем более новую версию, которую уже скачало автообновление.
    if has_engine && !crate::updates::newer(&src_ver, &dst_ver) {
        return Ok(());
    }
    kill_stray();
    log.push(format!("[prism] установка файлов zapret {}", src_ver.trim()));
    copy_dir(resource, target).map_err(|e| format!("Не удалось скопировать файлы zapret: {e}"))
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with("-user.txt") && to.exists() {
                continue;
            }
            fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_ipset_stub() {
        let dir = std::env::temp_dir().join("prism-ipset-test");
        let l = dir.join("lists");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&l).unwrap();
        let paths = ZapretPaths { root: dir.clone() };
        let read = |name: &str| fs::read_to_string(l.join(name)).unwrap();
        // Prism 0.10 скопировал полный список в ipset-all.txt: возвращаем заглушку.
        fs::write(l.join("ipset-all.txt"), "1.0.0.0/24\r\n8.8.8.0/24\r\n").unwrap();
        fs::write(l.join(GAMES_IPSET), "1.0.0.0/24\r\n8.8.8.0/24\r\n").unwrap();
        ensure_user_lists(&paths);
        assert_eq!(read("ipset-all.txt").trim(), IPSET_STUB);
        assert!(read(GAMES_IPSET).contains("8.8.8.0/24"));
        // Полного списка нет в .backup: переносим его туда, а не теряем.
        fs::remove_file(l.join(GAMES_IPSET)).unwrap();
        fs::write(l.join("ipset-all.txt"), "9.9.9.0/24\r\n").unwrap();
        ensure_user_lists(&paths);
        assert_eq!(read("ipset-all.txt").trim(), IPSET_STUB);
        assert!(read(GAMES_IPSET).contains("9.9.9.0/24"));
        // Заглушка остаётся заглушкой.
        ensure_user_lists(&paths);
        assert_eq!(read("ipset-all.txt").trim(), IPSET_STUB);
        assert!(l.join("list-general-user.txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_flowseal_bat() {
        let bat = "set \"BIN=%~dp0bin\\\"\r\nstart \"zapret: %~n0\" /min \"%BIN%winws.exe\" --wf-tcp=80,443,%GameFilterTCP% ^\r\n--filter-udp=443 --hostlist=\"%LISTS%list-general.txt\" --dpi-desync=fake --new ^\r\n--filter-tcp=80\r\n\r\necho done\r\n";
        let p = ZapretPaths { root: PathBuf::from("C:\\Data\\Prism Dir\\zapret") };
        let args = parse_bat(bat, &p, false).unwrap();
        assert_eq!(args[0], "--wf-tcp=80,443,12");
        assert_eq!(args[2].contains("Prism Dir"), true);
        assert!(args[2].ends_with("list-general.txt"));
        assert_eq!(args.last().unwrap(), "--filter-tcp=80");
        assert_eq!(tokenize("--fake-tls=^! --x=\"a b\""), vec!["--fake-tls=!", "--x=a b"]);
        let g = parse_bat(bat, &p, true).unwrap();
        assert_eq!(g[0], "--wf-tcp=80,443,1024-65535");
        let game = "set \"BIN=%~dp0bin\\\"\r\nstart \"z\" /min \"%BIN%winws.exe\" --wf-udp=443,%GameFilterUDP% ^\r\n--filter-udp=443 --ipset=\"%LISTS%ipset-all.txt\" --new ^\r\n--filter-udp=%GameFilterUDP% --ipset=\"%LISTS%ipset-all.txt\" --dpi-desync=fake --new ^\r\n--filter-tcp=80\r\n";
        let a = parse_bat(game, &p, true).unwrap();
        assert_eq!(a[0], "--wf-udp=443,1024-7769,7771-65535");
        assert!(a[2].ends_with("\\ipset-all.txt"));
        assert_eq!(a[4], "--filter-udp=1024-7769,7771-65535");
        assert!(a[5].ends_with(GAMES_IPSET));
        assert!(a[7].ends_with(GAMES_EXCLUDE));
        assert_eq!(a[8], "--new");
        let off = parse_bat(game, &p, false).unwrap();
        assert_eq!(off[0], "--wf-udp=443,12");
        assert!(off.iter().all(|x| !x.contains(GAMES_IPSET) && !x.contains(GAMES_EXCLUDE)));
    }

    #[test]
    fn adds_youtube_to_quic_hostlist() {
        let dir = std::env::temp_dir().join("prism-quic-test");
        let l = dir.join("lists");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&l).unwrap();
        fs::write(l.join("list-google.txt"), "googlevideo.com\r\n").unwrap();
        let p = ZapretPaths { root: dir.clone() };
        let google = format!("--hostlist={}\\list-google.txt", p.lists().display());
        let head = "start \"z\" /min \"%BIN%winws.exe\" --wf-udp=443 ^\r\n";
        let tail = "--filter-tcp=443 --hostlist=\"%LISTS%list-google.txt\" --dpi-desync=multisplit --new ^\r\n--filter-udp=443 --ipset=\"%LISTS%ipset-all.txt\" --dpi-desync=fake\r\n";
        for filter in ["--filter-udp=443", "--filter-l7=quic"] {
            let bat = format!("{head}{filter} --hostlist=\"%LISTS%list-general.txt\" --hostlist=\"%LISTS%list-general-user.txt\" --hostlist-exclude=\"%LISTS%list-exclude.txt\" --dpi-desync=fake --new ^\r\n{tail}");
            let a = parse_bat(&bat, &p, false).unwrap();
            assert_eq!(a[1], filter);
            assert!(a[3].ends_with("list-general-user.txt"));
            assert_eq!(a[4], google);
            assert!(a[5].starts_with("--hostlist-exclude="));
            // В правила для TCP и QUIC по IP ничего не добавляется.
            assert_eq!(a.iter().filter(|x| x.ends_with("list-google.txt")).count(), 2);
            assert_eq!(a.last().unwrap(), "--dpi-desync=fake");
            assert_eq!(a.len(), 15);
        }
        // Если list-google.txt уже есть в правиле QUIC, второй раз не добавляем.
        let bat = format!("{head}--filter-udp=443 --hostlist=\"%LISTS%list-general.txt\" --hostlist=\"%LISTS%list-google.txt\" --new ^\r\n{tail}");
        let a = parse_bat(&bat, &p, false).unwrap();
        assert_eq!(a.iter().filter(|x| **x == google).count(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_conflicting_services() {
        let en = "\r\nSERVICE_NAME: KillerNetworkService\r\nDISPLAY_NAME: Killer Network Service\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n\r\nSERVICE_NAME: nordvpn-service\r\nDISPLAY_NAME: NordVPN Service\r\n        STATE              : 4  RUNNING\r\n\r\nSERVICE_NAME: Dnscache\r\nDISPLAY_NAME: DNS Client\r\n";
        assert_eq!(conflicts(en), vec!["Killer Network Service (Intel Killer)"]);
        assert_eq!(vpn_services(en), vec!["nordvpn-service"]);
        // В русской Windows названия полей переведены.
        let ru = "Имя_службы: ProtonVPN Service\r\nВыводимое_имя: Proton VPN\r\n        Тип                : 10  WIN32_OWN_PROCESS\r\n\r\nИмя_службы: HappService\r\nВыводимое_имя: Happ\r\n\r\nИмя_службы: Dnscache\r\nВыводимое_имя: DNS-клиент\r\n";
        assert!(conflicts(ru).is_empty());
        assert_eq!(vpn_services(ru), vec!["ProtonVPN Service", "HappService"]);
        assert!(hosts_youtube("127.0.0.1 localhost\r\n1.2.3.4 www.youtube.com\r\n"));
        assert!(!hosts_youtube("# 1.2.3.4 youtube.com\r\n5.6.7.8 chatgpt.com\r\n"));
    }
}
