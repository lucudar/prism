#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod hosts;
mod probe;
mod settings;
mod tgproxy;
mod updates;
mod util;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};

use engine::{Engine, Strategy, ZapretPaths};
use settings::Settings;
use tgproxy::Proxy;
use util::{run_quiet, Log};

const ZAPRET_VERSION: &str = "обходы: Flowseal zapret-discord-youtube";
const TG_VERSION: &str = "tg-ws-proxy 1.11.1";
const TASK_NAME: &str = "Prism";

struct Inner {
    settings: Settings,
    engine: Engine,
    proxy: Proxy,
    busy: bool,
}

struct Prism {
    inner: Mutex<Inner>,
    log: Log,
    zapret: ZapretPaths,
    tg_exe: PathBuf,
    settings_file: PathBuf,
    hosts_cache: PathBuf,
    /// Последний итог проверки DNS для YouTube: в журнал пишем, только когда он меняется.
    dns_note: Mutex<&'static str>,
}

impl Prism {
    fn save(&self, i: &Inner) {
        i.settings.save(&self.settings_file);
    }

    /// Записать в журнал итог проверки DNS. Проверка на главной идёт каждую минуту,
    /// поэтому строки повторяются, только когда итог изменился или их просит подбор.
    fn note_dns(&self, d: &probe::DnsCheck, always: bool) {
        let mut last = self.dns_note.lock().unwrap();
        if always || *last != d.kind {
            *last = d.kind;
            for line in &d.lines {
                self.log.push(line.clone());
            }
        }
    }
}

#[derive(Serialize)]
struct TgInfo {
    host: &'static str,
    port: u16,
    secret: String,
    link: String,
}

#[derive(Serialize)]
struct AppState {
    engine_on: bool,
    /// Когда включился обход, миллисекунды с 1970 года (0 — выключен).
    since: u64,
    strategy: String,
    strategies: Vec<Strategy>,
    games: bool,
    ai: bool,
    tg_on: bool,
    tg: TgInfo,
    autostart: bool,
    tray: bool,
    user_sites: Vec<String>,
    builtin_sites: usize,
    versions: Vec<&'static str>,
    auto_update: bool,
    app_version: &'static str,
    zapret_version: String,
    portable: bool,
}

type R<T> = Result<T, String>;

fn snapshot(p: &Prism) -> AppState {
    let mut i = p.inner.lock().unwrap();
    let engine_on = i.engine.alive();
    let since = i.engine.since_ms();
    let tg_on = i.proxy.alive();
    let s = &i.settings;
    AppState {
        engine_on,
        since,
        strategy: s.strategy.clone(),
        strategies: engine::list_strategies(&p.zapret),
        games: s.games,
        ai: s.ai,
        tg_on,
        tg: TgInfo { host: tgproxy::HOST, port: s.tg_port, secret: s.tg_secret.clone(), link: tgproxy::link(s.tg_port, &s.tg_secret) },
        autostart: s.autostart,
        tray: s.tray,
        user_sites: read_user_sites(&p.zapret),
        builtin_sites: count_lines(&p.zapret.lists().join("list-general.txt")),
        versions: vec![ZAPRET_VERSION, TG_VERSION],
        auto_update: s.auto_update,
        app_version: env!("CARGO_PKG_VERSION"),
        zapret_version: zapret_version(&p.zapret),
        portable: is_portable(),
    }
}

fn count_lines(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .map(|t| t.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).count())
        .unwrap_or(0)
}

const PLACEHOLDER: &str = "domain.example.abc";

fn read_user_sites(z: &ZapretPaths) -> Vec<String> {
    std::fs::read_to_string(z.user_list())
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#') && l != PLACEHOLDER)
        .collect()
}

fn write_user_sites(z: &ZapretPaths, sites: &[String]) -> R<()> {
    let mut body = String::from("# Never leave this file empty\r\n");
    body.push_str(PLACEHOLDER);
    body.push_str("\r\n");
    for s in sites {
        body.push_str(s);
        body.push_str("\r\n");
    }
    std::fs::write(z.user_list(), body).map_err(|e| format!("Не удалось сохранить список: {e}"))
}

/// Запустить движок и убедиться, что он не упал сразу.
async fn start_engine(p: &Prism) -> R<()> {
    {
        let mut i = p.inner.lock().unwrap();
        let (strategy, games) = (i.settings.strategy.clone(), i.settings.games);
        i.engine.start(&p.zapret, &strategy, games, &p.log)?;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mut i = p.inner.lock().unwrap();
    if !i.engine.alive() {
        i.engine.stop();
        return Err("Обход не запустился. Откройте журнал в настройках, там написана причина.".into());
    }
    Ok(())
}


fn zapret_version(z: &ZapretPaths) -> String {
    std::fs::read_to_string(z.root.join("version.txt"))
        .unwrap_or_default()
        .trim()
        .trim_start_matches("flowseal-")
        .to_string()
}

/// Установленная версия лежит рядом с uninstall.exe, портативная — нет.
fn is_portable() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| !d.join("uninstall.exe").exists()))
        .unwrap_or(true)
}

fn open_url(url: &str) {
    let _ = util::hidden("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
}

async fn check_info(p: &Prism) -> R<updates::Info> {
    let (app_r, zap_r) = tokio::join!(updates::latest(updates::APP_REPO), updates::latest(updates::ZAPRET_REPO));
    let cur_app = env!("CARGO_PKG_VERSION").to_string();
    let cur_zap = zapret_version(&p.zapret);
    let mut info = updates::Info::default();
    info.app.current = cur_app.clone();
    info.zapret.current = cur_zap.clone();
    if let Ok(r) = &app_r {
        info.app.latest = r.tag_name.trim_start_matches('v').to_string();
        info.app.available = updates::newer(&r.tag_name, &cur_app) && updates::app_asset(r).is_some();
    }
    if let Ok(r) = &zap_r {
        info.zapret.latest = r.tag_name.trim_start_matches('v').to_string();
        info.zapret.available = updates::newer(&r.tag_name, &cur_zap) && updates::zapret_asset(r).is_some();
    }
    if let (Err(e), Err(_)) = (&app_r, &zap_r) {
        return Err(e.clone());
    }
    Ok(info)
}

/// Скачать и установить новую версию обходов Flowseal. Возвращает true, если обновились.
async fn do_update_zapret(p: &Prism) -> R<bool> {
    let rel = updates::latest(updates::ZAPRET_REPO).await?;
    let current = zapret_version(&p.zapret);
    if !updates::newer(&rel.tag_name, &current) {
        return Ok(false);
    }
    let asset = updates::zapret_asset(&rel).ok_or("В релизе Flowseal нет архива zapret")?.clone();
    p.log.push(format!("[prism] скачиваю обходы Flowseal {}", rel.tag_name));
    let bytes = updates::download_verified(&asset).await?;
    let root = p.zapret.root.clone();
    let fresh = root.with_file_name("zapret-new");
    let old = root.with_file_name("zapret-old");
    updates::unpack_zapret(&bytes, &fresh, &rel.tag_name)?;
    updates::carry_user_lists(&root, &fresh);
    let running = {
        let mut i = p.inner.lock().unwrap();
        let r = i.engine.alive();
        i.engine.stop();
        r
    };
    engine::kill_stray();
    engine::unload_driver();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(&root, &old).map_err(|e| format!("Не удалось заменить файлы: {e}"))?;
    if let Err(e) = std::fs::rename(&fresh, &root) {
        let _ = std::fs::rename(&old, &root);
        return Err(format!("Не удалось заменить файлы: {e}"));
    }
    let _ = std::fs::remove_dir_all(&old);
    {
        let mut i = p.inner.lock().unwrap();
        if !p.zapret.strategies().join(&i.settings.strategy).exists() {
            i.settings.strategy = "general.bat".into();
            p.save(&i);
        }
    }
    p.log.push(format!("[prism] обходы обновлены до {}", rel.tag_name));
    if running {
        start_engine(p).await?;
    }
    Ok(true)
}

async fn refresh_ai_hosts(p: &Prism) -> R<()> {
    let list = hosts::download().await?;
    let _ = std::fs::write(&p.hosts_cache, &list);
    hosts::enable(&list)?;
    p.log.push("[prism] список адресов для нейросетей обновлён");
    Ok(())
}

#[tauri::command]
async fn check_updates(p: State<'_, Prism>) -> R<updates::Info> {
    check_info(&p).await
}

#[tauri::command]
async fn update_zapret(p: State<'_, Prism>) -> R<AppState> {
    if !do_update_zapret(&p).await? {
        return Err("У вас уже последняя версия обходов".into());
    }
    Ok(snapshot(&p))
}

#[tauri::command]
async fn update_app(app: AppHandle, p: State<'_, Prism>) -> R<()> {
    let rel = updates::latest(updates::APP_REPO).await?;
    if !updates::newer(&rel.tag_name, env!("CARGO_PKG_VERSION")) {
        return Err("У вас уже последняя версия Prism".into());
    }
    if is_portable() {
        open_url(&rel.html_url);
        return Ok(());
    }
    let asset = updates::app_asset(&rel).ok_or("В релизе нет установщика")?.clone();
    p.log.push(format!("[prism] скачиваю Prism {}", rel.tag_name));
    let bytes = updates::download_verified(&asset).await?;
    let path = std::env::temp_dir().join(&asset.name);
    std::fs::write(&path, bytes).map_err(|e| format!("Не удалось сохранить установщик: {e}"))?;
    std::process::Command::new(&path).spawn().map_err(|e| format!("Не удалось запустить установщик: {e}"))?;
    shutdown(&app);
    app.exit(0);
    Ok(())
}

#[tauri::command]
async fn set_auto_update(p: State<'_, Prism>, on: bool) -> R<AppState> {
    {
        let mut i = p.inner.lock().unwrap();
        i.settings.auto_update = on;
        p.save(&i);
    }
    Ok(snapshot(&p))
}

#[tauri::command]
async fn get_state(p: State<'_, Prism>) -> R<AppState> {
    Ok(snapshot(&p))
}

#[tauri::command]
async fn set_engine(p: State<'_, Prism>, on: bool) -> R<AppState> {
    if on {
        start_engine(&p).await?;
    } else {
        p.inner.lock().unwrap().engine.stop();
        engine::kill_stray();
        engine::unload_driver();
    }
    {
        let mut i = p.inner.lock().unwrap();
        i.settings.engine_on = on;
        p.save(&i);
    }
    Ok(snapshot(&p))
}

#[tauri::command]
async fn set_strategy(p: State<'_, Prism>, id: String) -> R<AppState> {
    let running = {
        let mut i = p.inner.lock().unwrap();
        i.settings.strategy = id;
        p.save(&i);
        i.engine.alive()
    };
    if running {
        start_engine(&p).await?;
    }
    Ok(snapshot(&p))
}

#[tauri::command]
async fn set_games(p: State<'_, Prism>, on: bool) -> R<AppState> {
    let running = {
        let mut i = p.inner.lock().unwrap();
        i.settings.games = on;
        p.save(&i);
        i.engine.alive()
    };
    if running {
        start_engine(&p).await?;
    }
    Ok(snapshot(&p))
}

#[tauri::command]
async fn set_ai(p: State<'_, Prism>, on: bool) -> R<AppState> {
    if on {
        let list = match hosts::download().await {
            Ok(l) => {
                let _ = std::fs::write(&p.hosts_cache, &l);
                l
            }
            Err(e) => std::fs::read_to_string(&p.hosts_cache).map_err(|_| e)?,
        };
        hosts::enable(&list)?;
    } else {
        hosts::disable()?;
    }
    {
        let mut i = p.inner.lock().unwrap();
        i.settings.ai = on;
        p.save(&i);
    }
    p.log.push(format!("[prism] нейросети: {}", if on { "включено" } else { "выключено" }));
    Ok(snapshot(&p))
}

#[tauri::command]
async fn set_tg(p: State<'_, Prism>, on: bool) -> R<AppState> {
    {
        let mut i = p.inner.lock().unwrap();
        if on {
            if i.settings.tg_secret.len() != 32 {
                i.settings.tg_secret = tgproxy::new_secret();
            }
            let (port, secret) = (i.settings.tg_port, i.settings.tg_secret.clone());
            i.proxy.start(&p.tg_exe, port, &secret, &p.log)?;
        } else {
            i.proxy.stop();
        }
        i.settings.tg_on = on;
        p.save(&i);
    }
    if on {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let mut i = p.inner.lock().unwrap();
        if !i.proxy.alive() {
            i.settings.tg_on = false;
            p.save(&i);
            return Err("Прокси не запустился. Возможно, порт занят другой программой. Подробности в журнале.".into());
        }
    }
    Ok(snapshot(&p))
}

#[tauri::command]
async fn open_tg_link(p: State<'_, Prism>) -> R<()> {
    let link = {
        let i = p.inner.lock().unwrap();
        tgproxy::link(i.settings.tg_port, &i.settings.tg_secret)
    };
    // rundll32 открывает ссылку tg:// в Telegram без участия cmd (символ & не ломается).
    util::hidden("rundll32").args(["url.dll,FileProtocolHandler", &link]).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

fn normalize_domain(raw: &str) -> Option<String> {
    let mut d = raw.trim().to_lowercase();
    for pre in ["https://", "http://"] {
        if let Some(rest) = d.strip_prefix(pre) {
            d = rest.to_string();
        }
    }
    let d = d.split(['/', '?', '#', ':']).next().unwrap_or("").trim_start_matches("www.").to_string();
    let valid = d.contains('.')
        && !d.starts_with('.')
        && !d.ends_with('.')
        && d.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    valid.then_some(d)
}

#[tauri::command]
async fn add_site(p: State<'_, Prism>, domain: String) -> R<AppState> {
    let d = normalize_domain(&domain).ok_or("Введите адрес сайта, например site.ru")?;
    let mut sites = read_user_sites(&p.zapret);
    if sites.contains(&d) {
        return Err(format!("{d} уже в списке"));
    }
    sites.insert(0, d);
    write_user_sites(&p.zapret, &sites)?;
    Ok(snapshot(&p))
}

#[tauri::command]
async fn remove_site(p: State<'_, Prism>, domain: String) -> R<AppState> {
    let sites: Vec<String> = read_user_sites(&p.zapret).into_iter().filter(|s| *s != domain).collect();
    write_user_sites(&p.zapret, &sites)?;
    Ok(snapshot(&p))
}

#[derive(Serialize)]
struct Status {
    discord: probe::Check,
    youtube: probe::Check,
    telegram: probe::Check,
    chatgpt: bool,
    /// YouTube открывается только по адресу от безопасного DNS: в браузере
    /// с DNS провайдера он пишет «Нет подключения к интернету».
    youtube_dns: bool,
}

#[tauri::command]
async fn check_services(p: State<'_, Prism>) -> R<Status> {
    let (port, ai, on) = {
        let mut i = p.inner.lock().unwrap();
        let on = i.engine.alive();
        (i.settings.tg_port, i.settings.ai, on)
    };
    let dns = probe::Dns::cached().await;
    // Адреса YouTube сравниваем только с включённым обходом: без него
    // YouTube не открывается ни по какому адресу.
    let dns_check = async {
        if on {
            Some(probe::youtube_dns(&dns, 6).await)
        } else {
            None
        }
    };
    let (s, d) = tokio::join!(probe::services(6, &dns), dns_check);
    let telegram = probe::tcp(port).await;
    let chatgpt = ai && std::fs::read_to_string(hosts::hosts_path()).map(|t| t.contains("Prism: нейросети")).unwrap_or(false);
    let youtube_dns = d
        .map(|d| {
            p.note_dns(&d, false);
            d.bad
        })
        .unwrap_or(false);
    Ok(Status { discord: s.discord, youtube: s.youtube, telegram, chatgpt, youtube_dns })
}

#[derive(Clone, Serialize)]
struct Progress {
    id: String,
    state: &'static str,
    ms: u64,
}

#[tauri::command]
async fn auto_pick(app: AppHandle, p: State<'_, Prism>) -> R<AppState> {
    {
        let mut i = p.inner.lock().unwrap();
        if i.busy {
            return Err("Подбор уже идёт".into());
        }
        i.busy = true;
    }
    let list = engine::list_strategies(&p.zapret);
    let previous = p.inner.lock().unwrap().settings.strategy.clone();
    // YouTube проверяем по адресам от безопасного DNS: так стратегии сравниваются
    // по тому, как они обходят блокировку, а не по ответу DNS провайдера.
    let dns = probe::Dns::lookup().await;
    p.log.push(if dns.is_empty() {
        "[prism] безопасный DNS не ответил, YouTube проверяется по адресам от DNS Windows"
    } else {
        "[prism] YouTube проверяется по адресам от безопасного DNS (Google, Cloudflare)"
    });
    let mut best: Option<(usize, u64, String)> = None;
    for s in &list {
        let _ = app.emit("pick-progress", Progress { id: s.id.clone(), state: "wait", ms: 0 });
        p.inner.lock().unwrap().settings.strategy = s.id.clone();
        let started = start_engine(&p).await.is_ok();
        // Оценка важнее «работает/не работает»: для YouTube считаем, сколько адресов
        // ответило из проверенных. Стратегия, где открывается сайт, но не видео, хуже.
        let (score, ms, failed_yt) = if started {
            let mut r = probe::services(4, &dns).await;
            // Сразу после перезапуска движка часть соединений ещё не поднялась.
            // Нулевой результат повторяем один раз, чтобы не отбросить рабочую стратегию.
            if !r.discord.ok && !r.youtube.ok {
                tokio::time::sleep(Duration::from_millis(900)).await;
                r = probe::services(4, &dns).await;
            }
            let svc = [&r.discord, &r.youtube];
            let ok = svc.iter().filter(|x| x.ok).count();
            let adr = svc.iter().map(|x| x.good as usize).sum::<usize>();
            let ms = if ok > 0 { (r.discord.ms + r.youtube.ms) / ok as u64 } else { 0 };
            // Что именно не открылось и почему: сайт, видеосервер или api.
            let failed = if r.youtube.total > r.youtube.good {
                probe::youtube_failed(&dns.client(4)).await
            } else {
                Vec::new()
            };
            (ok * 100 + adr, ms, failed.join(", "))
        } else {
            (0, 0, String::new())
        };
        let state = if score >= 200 { "ok" } else if score > 0 { "part" } else { "bad" };
        let _ = app.emit("pick-progress", Progress { id: s.id.clone(), state, ms });
        p.log.push(format!(
            "[prism] {}: оценка {score}, {ms} мс{}",
            s.name,
            if failed_yt.is_empty() { String::new() } else { format!(", YouTube не открылся: {failed_yt}") }
        ));
        let better = match &best {
            None => score > 0,
            Some((bs, bms, _)) => score > *bs || (score == *bs && ms < *bms),
        };
        if better {
            best = Some((score, ms, s.id.clone()));
        }
    }
    let chosen = best.map(|b| b.2).unwrap_or(previous);
    {
        let mut i = p.inner.lock().unwrap();
        i.settings.strategy = chosen;
        i.settings.engine_on = true;
        i.busy = false;
        p.save(&i);
    }
    let result = start_engine(&p).await;
    result?;
    // Итог для журнала: открывается ли YouTube по адресу от DNS Windows.
    p.note_dns(&probe::youtube_dns(&dns, 5).await, true);
    Ok(snapshot(&p))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// Задание планировщика для автозапуска: при входе в Windows, с правами администратора и без окна UAC.
/// В отличие от простого `schtasks /SC ONLOGON` оно запускается и от батареи,
/// не останавливается через 3 дня и работает с обычным приоритетом.
fn task_xml(exe: &std::path::Path) -> String {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) if !d.is_empty() && !u.is_empty() => format!("{d}\\{u}"),
        (_, Ok(u)) => u,
        _ => String::new(),
    };
    let user = if user.is_empty() { String::new() } else { format!("<UserId>{}</UserId>", xml_escape(&user)) };
    let dir = exe.parent().map(|d| d.display().to_string()).unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Запускает Prism при входе в Windows и сразу включает обход</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      {user}
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      {user}
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{cmd}</Command>
      <Arguments>--autostart</Arguments>
      <WorkingDirectory>{dir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#,
        user = user,
        cmd = xml_escape(&exe.display().to_string()),
        dir = xml_escape(&dir),
    )
}

fn set_autostart_task(on: bool, log: &Log) -> R<()> {
    if !on {
        run_quiet("schtasks", &["/Delete", "/F", "/TN", TASK_NAME]);
        log.push("[prism] автозапуск выключен");
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // schtasks читает XML в UTF-16 с меткой порядка байтов, как его сохраняет сам планировщик.
    let mut bytes = vec![0xFF, 0xFE];
    for unit in task_xml(&exe).encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let file = std::env::temp_dir().join("prism-autostart.xml");
    let file_s = file.display().to_string();
    let ok = std::fs::write(&file, &bytes).is_ok()
        && run_quiet("schtasks", &["/Create", "/F", "/TN", TASK_NAME, "/XML", &file_s]);
    let _ = std::fs::remove_file(&file);
    if ok {
        log.push("[prism] автозапуск включён: Prism запустится при входе в Windows и включит обход");
        return Ok(());
    }
    // Запасной вариант: простое задание при входе в систему.
    let tr = format!("\"{}\" --autostart", exe.display());
    if run_quiet("schtasks", &["/Create", "/F", "/TN", TASK_NAME, "/TR", &tr, "/SC", "ONLOGON", "/RL", "HIGHEST"]) {
        log.push("[prism] автозапуск включён (простое задание планировщика)");
        return Ok(());
    }
    Err("Не удалось включить автозапуск. Подробности в журнале.".into())
}

#[tauri::command]
async fn set_autostart(p: State<'_, Prism>, on: bool) -> R<AppState> {
    set_autostart_task(on, &p.log)?;
    {
        let mut i = p.inner.lock().unwrap();
        i.settings.autostart = on;
        p.save(&i);
    }
    Ok(snapshot(&p))
}

#[tauri::command]
async fn set_tray(p: State<'_, Prism>, on: bool) -> R<AppState> {
    {
        let mut i = p.inner.lock().unwrap();
        i.settings.tray = on;
        p.save(&i);
    }
    Ok(snapshot(&p))
}

#[tauri::command]
async fn get_log(p: State<'_, Prism>) -> R<String> {
    Ok(p.log.text())
}

#[tauri::command]
async fn close_window(app: AppHandle, p: State<'_, Prism>) -> R<()> {
    let tray = p.inner.lock().unwrap().settings.tray;
    if tray {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.hide();
        }
    } else {
        shutdown(&app);
        app.exit(0);
    }
    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn shutdown(app: &AppHandle) {
    if let Some(p) = app.try_state::<Prism>() {
        let mut i = p.inner.lock().unwrap();
        i.engine.stop();
        i.proxy.stop();
    }
    engine::kill_stray();
    engine::unload_driver();
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let log = Log::default();
    let res = app.path().resource_dir()?.join("resources");
    let data = app.path().app_local_data_dir()?;
    std::fs::create_dir_all(&data)?;
    let zapret = ZapretPaths { root: data.join("zapret") };
    if let Err(e) = engine::install(&res.join("zapret"), &zapret.root, &log) {
        log.push(format!("[prism] {e}"));
    }
    // Программы, которые мешают zapret, Prism только называет в журнале.
    let diag = log.clone();
    std::thread::spawn(move || engine::diagnose(&diag));
    let settings_file = data.join("settings.json");
    let mut settings = Settings::load(&settings_file);
    if settings.tg_secret.len() != 32 {
        settings.tg_secret = tgproxy::new_secret();
    }
    // Запуск при входе в Windows: всё должно заработать сразу, поэтому обход включается,
    // даже если в прошлый раз его выключили вручную.
    let autostart = std::env::args().any(|a| a == "--autostart");
    if autostart {
        settings.engine_on = true;
        log.push("[prism] запуск вместе с Windows");
    }
    settings.save(&settings_file);
    let restore = (settings.engine_on, settings.tg_on);
    let keep_task = settings.autostart;

    app.manage(Prism {
        inner: Mutex::new(Inner { settings, engine: Engine::default(), proxy: Proxy::default(), busy: false }),
        log,
        zapret,
        tg_exe: res.join("tgproxy").join("tg-ws-proxy.exe"),
        settings_file,
        hosts_cache: data.join("ai-hosts.txt"),
        dns_note: Mutex::new(""),
    });

    // Трей
    let show = MenuItem::with_id(app, "show", "Открыть Prism", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выйти", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().ok_or("no icon")?)
        .tooltip("Prism")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "show" => show_main(app),
            "quit" => {
                shutdown(app);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;

    if !autostart {
        show_main(app.handle());
    }

    // Подсказка у значка в трее: видно, включён ли обход, даже когда окно скрыто.
    let tip = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let mut last: Option<bool> = None;
        loop {
            let on = tip.state::<Prism>().inner.lock().unwrap().engine.alive();
            if last != Some(on) {
                if let Some(t) = tip.tray_by_id("main") {
                    let text = if on { "Prism: обход включён" } else { "Prism: обход выключен" };
                    if t.set_tooltip(Some(text)).is_ok() {
                        last = Some(on);
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    // Вернуть то, что было включено в прошлый раз.
    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let p = handle.state::<Prism>();
        if restore.0 {
            if let Err(e) = start_engine(&p).await {
                p.log.push(format!("[prism] {e}"));
            }
        }
        if restore.1 {
            let mut i = p.inner.lock().unwrap();
            let (port, secret) = (i.settings.tg_port, i.settings.tg_secret.clone());
            if let Err(e) = i.proxy.start(&p.tg_exe, port, &secret, &p.log) {
                p.log.push(format!("[prism] {e}"));
            }
        }
        let _ = handle.emit("state-changed", ());

        // Обновить задание автозапуска: путь к программе мог измениться,
        // а задания из старых версий не запускались от батареи.
        if keep_task {
            if let Err(e) = set_autostart_task(true, &p.log) {
                p.log.push(format!("[prism] {e}"));
            }
        }

        // Фоновые обновления через несколько секунд после запуска.
        tokio::time::sleep(Duration::from_secs(8)).await;
        let (auto, ai) = {
            let i = p.inner.lock().unwrap();
            (i.settings.auto_update, i.settings.ai)
        };
        if auto {
            match do_update_zapret(&p).await {
                Ok(true) => {
                    let _ = handle.emit("zapret-updated", zapret_version(&p.zapret));
                    let _ = handle.emit("state-changed", ());
                }
                Ok(false) => {}
                Err(e) => p.log.push(format!("[prism] автообновление обходов: {e}")),
            }
            if ai {
                if let Err(e) = refresh_ai_hosts(&p).await {
                    p.log.push(format!("[prism] {e}"));
                }
            }
        }
        if let Ok(info) = check_info(&p).await {
            let _ = handle.emit("updates", info);
        }
    });
    Ok(())
}

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .setup(setup)
        .on_window_event(|w, e| {
            if let WindowEvent::CloseRequested { api, .. } = e {
                let tray = w.app_handle().try_state::<Prism>().map(|p| p.inner.lock().unwrap().settings.tray).unwrap_or(false);
                if tray {
                    api.prevent_close();
                    let _ = w.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state, set_engine, set_strategy, set_games, set_ai, set_tg, open_tg_link,
            add_site, remove_site, check_services, auto_pick, set_autostart, set_tray, get_log, close_window,
            check_updates, update_zapret, update_app, set_auto_update
        ])
        .build(tauri::generate_context!())
        .expect("ошибка запуска Prism");

    app.run(|app, e| {
        if let RunEvent::Exit = e {
            shutdown(app);
        }
    });
}
