#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cleanup;
mod engine;
mod hosts;
mod net;
mod probe;
mod secure;
mod settings;
mod tgproxy;
mod updates;
mod util;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::window::{Effect, EffectsBuilder};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};

use engine::{Engine, Strategy, ZapretPaths};
use settings::Settings;
use tgproxy::Proxy;
use util::{run_quiet, Log};

const ZAPRET_VERSION: &str = "обходы: Flowseal zapret-discord-youtube";
/// Версия прокси, если сборка не положила рядом с ним version.txt.
const TG_VERSION: &str = "tg-ws-proxy 1.11.1";
const TASK_NAME: &str = "Prism";

/// Чем занят движок. Тексты уходят в окно, если команду пришлось отклонить.
const PICKING: &str = "Идёт подбор стратегии";
const UPDATING: &str = "Обновляются обходы";
const RESTARTING: &str = "Обход перезапускается";
/// Сторож перезапускает упавший winws.exe не чаще стольких раз за 5 минут.
const MAX_RESTARTS: usize = 3;
/// Сколько своих сайтов проверяет подбор и сколько очков даёт каждый открывшийся.
/// Discord и YouTube дают по 100, поэтому свои сайты решают спор, но не перевешивают их.
const OWN_SITES: usize = 3;
const OWN_SITE_SCORE: usize = 30;

struct Inner {
    settings: Settings,
    engine: Engine,
    proxy: Proxy,
    /// Чем сейчас занят движок (см. Busy).
    busy: Option<&'static str>,
}

struct Prism {
    inner: Mutex<Inner>,
    log: Log,
    /// Защищённая папка в %ProgramData% (см. secure.rs).
    data: PathBuf,
    zapret: ZapretPaths,
    tg_exe: PathBuf,
    tg_version: String,
    settings_file: PathBuf,
    hosts_cache: PathBuf,
    /// Последний итог проверки DNS для YouTube: в журнал пишем, только когда он меняется.
    dns_note: Mutex<&'static str>,
    /// Нажали «Стоп» во время подбора.
    cancel: AtomicBool,
    /// Имя текущей сети (см. net.rs), пустое, пока не узнали или нет сети.
    network: Mutex<String>,
    /// Номер перемены сети. Подбор и профили сверяются с ним, чтобы результат,
    /// полученный в одной сети, не попал в профиль другой.
    net_gen: AtomicU64,
    /// Запись в hosts и флажок «Нейросети» меняются вместе, под этим замком.
    hosts_lock: Mutex<()>,
    /// Windows 11: окно может быть полупрозрачным (Mica).
    mica_ok: bool,
}

/// Движок занят: пока идёт подбор, обновление или перезапуск, другие команды
/// движка отказываются. Раньше во время подбора можно было переключить игровой режим
/// или обновить обходы, и движок перезапускался посреди проверки. Занятость
/// снимается сама, когда Busy выходит из области видимости, даже при ошибке.
struct Busy<'a>(&'a Prism);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.inner.lock().unwrap().busy = None;
    }
}

impl Prism {
    fn save(&self, i: &Inner) {
        i.settings.save(&self.settings_file);
    }

    /// Занять движок или сказать, чем он уже занят.
    fn begin(&self, what: &'static str) -> R<Busy<'_>> {
        let mut i = self.inner.lock().unwrap();
        if let Some(cur) = i.busy {
            return Err(if cur == PICKING {
                format!("{cur}. Дождитесь конца или нажмите «Стоп»")
            } else {
                format!("{cur}, подождите пару секунд")
            });
        }
        i.busy = Some(what);
        Ok(Busy(self))
    }

    /// Занять движок, дождавшись, пока он освободится. Для запуска Prism: обход
    /// нужно включить в любом случае, даже если первым движок занял профиль сети.
    async fn wait_begin(&self, what: &'static str) -> Busy<'_> {
        loop {
            if let Ok(b) = self.begin(what) {
                return b;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    fn network(&self) -> String {
        self.network.lock().unwrap().clone()
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
    versions: Vec<String>,
    auto_update: bool,
    app_version: &'static str,
    zapret_version: String,
    portable: bool,
    /// Имя текущей сети и запомнена ли для неё выбранная стратегия.
    network: String,
    profile: bool,
    mica: bool,
    mica_ok: bool,
}

type R<T> = Result<T, String>;

fn snapshot(p: &Prism) -> AppState {
    let network = p.network();
    let mut i = p.inner.lock().unwrap();
    let engine_on = i.engine.alive();
    let since = i.engine.since_ms();
    let tg_on = i.proxy.alive();
    let s = &i.settings;
    let profile = !network.is_empty() && s.profile(&network) == Some(s.strategy.as_str());
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
        versions: vec![ZAPRET_VERSION.to_string(), p.tg_version.clone()],
        auto_update: s.auto_update,
        app_version: env!("CARGO_PKG_VERSION"),
        zapret_version: zapret_version(&p.zapret),
        portable: is_portable(),
        network,
        profile,
        mica: s.mica,
        mica_ok: p.mica_ok,
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
    util::write_atomic(&z.user_list(), body).map_err(|e| format!("Не удалось сохранить список: {e}"))
}

/// Запустить движок и убедиться, что он не упал сразу.
/// Вызывается только тот, кто занял движок через Prism::begin.
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

/// Остановить движок и выгрузить драйвер.
fn stop_engine(p: &Prism) {
    p.inner.lock().unwrap().engine.stop();
    engine::kill_stray(&p.zapret);
    engine::unload_driver(&p.zapret);
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

/// Версия прокси Telegram из version.txt, который кладёт сборка.
fn tg_version(res: &Path) -> String {
    std::fs::read_to_string(res.join("tgproxy").join("version.txt"))
        .ok()
        .map(|v| v.trim().trim_start_matches('v').to_string())
        .filter(|v| !v.is_empty() && v.len() < 20)
        .map(|v| format!("tg-ws-proxy {v}"))
        .unwrap_or_else(|| TG_VERSION.to_string())
}

/// Номер сборки Windows (22000 и выше — Windows 11) из вывода `ver`.
fn windows_build() -> u32 {
    let out = util::hidden("cmd")
        .args(["/C", "ver"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    parse_build(&out)
}

/// «Microsoft Windows [Version 10.0.22631.4602]» → 22631. Слово «Version» в русской Windows
/// переведено, поэтому ищем сами числа через точку.
fn parse_build(s: &str) -> u32 {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|t| t.matches('.').count() >= 2)
        .and_then(|t| t.split('.').nth(2))
        .and_then(|b| b.parse().ok())
        .unwrap_or(0)
}

/// Эффект Mica: сквозь фон окна чуть видны обои. Только Windows 11.
fn apply_mica(app: &AppHandle, on: bool) {
    if let Some(w) = app.get_webview_window("main") {
        let fx = on.then(|| EffectsBuilder::new().effect(Effect::Mica).build());
        let _ = w.set_effects(fx);
    }
}

/// Серая и чуть прозрачная копия значка: так в трее видно, что обход выключен.
fn gray(rgba: &[u8]) -> Vec<u8> {
    let mut px = rgba.to_vec();
    for c in px.chunks_exact_mut(4) {
        let y = ((c[0] as u32 * 299 + c[1] as u32 * 587 + c[2] as u32 * 114) / 1000) as u8;
        c[0] = y;
        c[1] = y;
        c[2] = y;
        c[3] = (c[3] as u32 * 11 / 20) as u8;
    }
    px
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
    // Пока файл качается, движок свободен. Занимаем его только на замену файлов.
    let _busy = p.begin(UPDATING)?;
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
    engine::kill_stray(&p.zapret);
    engine::unload_driver(&p.zapret);
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

/// Записать адреса нейросетей, если доступ к ним всё ещё включён. Проверка флажка
/// и запись идут под одним замком с set_ai: иначе скачивание, закончившееся уже
/// после выключения, вернуло бы блок в hosts. Возвращает, записан ли блок.
fn apply_ai_hosts(p: &Prism, list: &str) -> R<bool> {
    let _g = p.hosts_lock.lock().unwrap();
    if !p.inner.lock().unwrap().settings.ai {
        return Ok(false);
    }
    hosts::enable(list)?;
    Ok(true)
}

async fn refresh_ai_hosts(p: &Prism) -> R<bool> {
    let list = hosts::download().await?;
    let _ = std::fs::write(&p.hosts_cache, &list);
    let written = apply_ai_hosts(p, &list)?;
    if written {
        p.log.push("[prism] список адресов для нейросетей обновлён");
    }
    Ok(written)
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
    // Не в %TEMP%: оттуда установщик, который запускается с правами администратора,
    // успела бы подменить любая программа пользователя.
    let dir = p.data.join("updates");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Не удалось сохранить установщик: {e}"))?;
    let path = dir.join("Prism-setup.exe");
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
async fn set_mica(app: AppHandle, p: State<'_, Prism>, on: bool) -> R<AppState> {
    if !p.mica_ok {
        return Err("Прозрачный фон работает только в Windows 11".into());
    }
    {
        let mut i = p.inner.lock().unwrap();
        i.settings.mica = on;
        p.save(&i);
    }
    apply_mica(&app, on);
    Ok(snapshot(&p))
}

#[tauri::command]
async fn get_state(p: State<'_, Prism>) -> R<AppState> {
    Ok(snapshot(&p))
}

#[tauri::command]
async fn set_engine(p: State<'_, Prism>, on: bool) -> R<AppState> {
    let _busy = p.begin(RESTARTING)?;
    if on {
        start_engine(&p).await?;
    } else {
        stop_engine(&p);
    }
    {
        let mut i = p.inner.lock().unwrap();
        i.settings.engine_on = on;
        p.save(&i);
    }
    Ok(snapshot(&p))
}

/// Ручной выбор стратегии запоминается для текущей сети, как и итог подбора.
#[tauri::command]
async fn set_strategy(p: State<'_, Prism>, id: String) -> R<AppState> {
    if !engine::list_strategies(&p.zapret).iter().any(|s| s.id == id) {
        return Err("Такой стратегии нет".into());
    }
    let net = p.network();
    let _busy = p.begin(RESTARTING)?;
    let running = {
        let mut i = p.inner.lock().unwrap();
        i.settings.remember(&net, &id);
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
    let _busy = p.begin(RESTARTING)?;
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
    let list = if on {
        Some(match hosts::download().await {
            Ok(l) => {
                let _ = std::fs::write(&p.hosts_cache, &l);
                l
            }
            Err(e) => std::fs::read_to_string(&p.hosts_cache).map_err(|_| e)?,
        })
    } else {
        None
    };
    {
        // Запись в hosts и флажок меняются вместе: см. apply_ai_hosts.
        let _g = p.hosts_lock.lock().unwrap();
        match &list {
            Some(l) => hosts::enable(l)?,
            None => hosts::disable()?,
        }
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
            if !tgproxy::valid_secret(&i.settings.tg_secret) {
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
    let chatgpt = ai && hosts::present();
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

/// Свои сайты из раздела «Сайты»: сколько из первых нескольких открылось.
/// Их добавляют как раз потому, что они не открываются, так что подбор их учитывает.
async fn own_sites(sites: &[String]) -> probe::Check {
    if sites.is_empty() {
        return probe::Check::default();
    }
    let owned: Vec<(String, String)> = sites.iter().take(OWN_SITES).map(|d| (d.clone(), format!("https://{d}/"))).collect();
    let urls: Vec<(&str, &str)> = owned.iter().map(|(n, u)| (n.as_str(), u.as_str())).collect();
    probe::any(&probe::client(4), &urls).await
}

#[tauri::command]
async fn auto_pick(app: AppHandle, p: State<'_, Prism>) -> R<AppState> {
    let _busy = p.begin(PICKING)?;
    p.cancel.store(false, Ordering::SeqCst);
    let stopped = || p.cancel.load(Ordering::SeqCst);
    // Номер сети, в которой идёт подбор: см. конец функции.
    let gen = p.net_gen.load(Ordering::SeqCst);
    let list = engine::list_strategies(&p.zapret);
    let user = read_user_sites(&p.zapret);
    let (previous, was_on) = {
        let i = p.inner.lock().unwrap();
        (i.settings.strategy.clone(), i.settings.engine_on)
    };
    // YouTube проверяем по адресам от безопасного DNS: так стратегии сравниваются
    // по тому, как они обходят блокировку, а не по ответу DNS провайдера.
    let dns = probe::Dns::lookup().await;
    p.log.push(if dns.is_empty() {
        "[prism] безопасный DNS не ответил, YouTube проверяется по адресам от DNS Windows"
    } else {
        "[prism] YouTube проверяется по адресам от безопасного DNS (Google, Cloudflare)"
    });
    if !user.is_empty() {
        let names: Vec<&str> = user.iter().take(OWN_SITES).map(String::as_str).collect();
        p.log.push(format!("[prism] подбор проверяет и свои сайты: {}", names.join(", ")));
    }
    let mut best: Option<(usize, u64, String)> = None;
    for s in &list {
        if stopped() {
            break;
        }
        let _ = app.emit("pick-progress", Progress { id: s.id.clone(), state: "wait", ms: 0 });
        p.inner.lock().unwrap().settings.strategy = s.id.clone();
        let started = start_engine(&p).await.is_ok();
        if stopped() {
            break;
        }
        // Оценка важнее «работает/не работает»: для YouTube считаем, сколько адресов
        // ответило из проверенных. Стратегия, где открывается сайт, но не видео, хуже.
        let (score, ms, failed_yt, own_note) = if started {
            let (mut r, mut own) = tokio::join!(probe::services(4, &dns), own_sites(&user));
            // Сразу после перезапуска движка часть соединений ещё не поднялась.
            // Нулевой результат повторяем один раз, чтобы не отбросить рабочую стратегию.
            if !r.discord.ok && !r.youtube.ok && !stopped() {
                tokio::time::sleep(Duration::from_millis(900)).await;
                (r, own) = tokio::join!(probe::services(4, &dns), own_sites(&user));
            }
            let svc = [&r.discord, &r.youtube];
            let ok = svc.iter().filter(|x| x.ok).count();
            let adr = svc.iter().map(|x| x.good as usize).sum::<usize>();
            let ms = if ok > 0 { (r.discord.ms + r.youtube.ms) / ok as u64 } else { 0 };
            let note = if own.total > 0 { format!(", свои сайты: {} из {}", own.good, own.total) } else { String::new() };
            // Что именно не открылось и почему: сайт, видеосервер или api.
            // Причины собраны в той же проверке, перепроверять YouTube не нужно.
            (ok * 100 + adr + own.good as usize * OWN_SITE_SCORE, ms, r.youtube.failed.join(", "), note)
        } else {
            (0, 0, String::new(), String::new())
        };
        let state = if score >= 200 { "ok" } else if score > 0 { "part" } else { "bad" };
        let _ = app.emit("pick-progress", Progress { id: s.id.clone(), state, ms });
        p.log.push(format!(
            "[prism] {}: оценка {score}, {ms} мс{own_note}{}",
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
    // «Стоп» берёт лучшую из уже проверенных. Если рабочей не нашлось,
    // возвращаем прежнюю стратегию и прежнее состояние обхода.
    let cancelled = stopped();
    let found = best.is_some();
    let (chosen, on) = match best {
        Some(b) => (b.2, true),
        None if cancelled => (previous, was_on),
        None => (previous, true),
    };
    if cancelled {
        p.log.push("[prism] подбор остановлен");
    }
    // Найденная стратегия запоминается для сети, в которой её проверяли. Если сеть
    // за время подбора сменилась, результат смешанный, и профили не трогаем.
    let net = p.network();
    let same_net = p.net_gen.load(Ordering::SeqCst) == gen;
    let keep = found && same_net && !net.is_empty();
    {
        let mut i = p.inner.lock().unwrap();
        if keep {
            i.settings.remember(&net, &chosen);
        }
        i.settings.strategy = chosen;
        i.settings.engine_on = on;
        p.save(&i);
    }
    if keep {
        p.log.push(format!("[prism] стратегия запомнена для сети «{net}»"));
    } else if found && !same_net {
        p.log.push("[prism] сеть сменилась во время подбора, стратегия для сети не запомнена");
    }
    if on {
        start_engine(&p).await?;
        if !cancelled {
            // Итог для журнала: открывается ли YouTube по адресу от DNS Windows.
            p.note_dns(&probe::youtube_dns(&dns, 5).await, true);
        }
    } else {
        stop_engine(&p);
    }
    Ok(snapshot(&p))
}

/// «Стоп» во время подбора: текущая проверка доходит до конца, дальше подбор не идёт.
#[tauri::command]
async fn stop_pick(p: State<'_, Prism>) -> R<()> {
    p.cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// Сеть поменялась: включить стратегию, которая в этой сети уже работала.
/// `gen` — номер перемены сети. Если сеть успела смениться ещё раз, этот вызов
/// ничего не делает: профилем займётся вызов для новой сети.
async fn apply_profile(app: AppHandle, gen: u64) {
    let p = app.state::<Prism>();
    let _ = app.emit("state-changed", ());
    let stale = || p.net_gen.load(Ordering::SeqCst) != gen;
    let net = p.network();
    // Профиля для этой сети нет: и ждать нечего.
    if p.inner.lock().unwrap().settings.profile(&net).is_none() {
        return;
    }
    // Движок может быть занят подбором, обновлением или запуском Prism: ждём,
    // сколько нужно. После новой перемены сети ждать незачем.
    let busy = loop {
        if stale() {
            return;
        }
        match p.begin(RESTARTING) {
            Ok(b) => break b,
            Err(_) => tokio::time::sleep(Duration::from_secs(3)).await,
        }
    };
    if stale() {
        return;
    }
    // Профиль читаем заново: пока ждали, подбор мог запомнить для этой сети другую.
    let want = p.inner.lock().unwrap().settings.profile(&net).map(str::to_string);
    let Some(want) = want else { return };
    let Some(strategy) = engine::list_strategies(&p.zapret).into_iter().find(|s| s.id == want) else { return };
    let (same, running) = {
        let mut i = p.inner.lock().unwrap();
        let same = i.settings.strategy == want;
        if !same {
            i.settings.strategy = want.clone();
            p.save(&i);
        }
        (same, i.engine.alive())
    };
    if same {
        return;
    }
    p.log.push(format!("[prism] сеть «{net}»: стратегия «{}» из профиля сети", strategy.name));
    let text = if !running {
        format!("Сеть «{net}»: выбрана стратегия «{}»", strategy.name)
    } else if let Err(e) = start_engine(&p).await {
        p.log.push(format!("[prism] {e}"));
        format!("Сеть «{net}»: стратегия «{}» не запустилась, подробности в журнале", strategy.name)
    } else {
        format!("Сеть «{net}»: включена стратегия «{}»", strategy.name)
    };
    drop(busy);
    let _ = app.emit("notice", text);
    let _ = app.emit("state-changed", ());
}

/// Включить или выключить обход из меню значка в трее.
async fn tray_toggle(app: AppHandle) {
    let p = app.state::<Prism>();
    let busy = match p.begin(RESTARTING) {
        Ok(b) => b,
        Err(e) => {
            let _ = app.emit("notice", e);
            return;
        }
    };
    let on = !p.inner.lock().unwrap().engine.alive();
    let r = if on {
        start_engine(&p).await
    } else {
        stop_engine(&p);
        Ok(())
    };
    match r {
        Ok(()) => {
            let mut i = p.inner.lock().unwrap();
            i.settings.engine_on = on;
            p.save(&i);
        }
        Err(e) => p.log.push(format!("[prism] {e}")),
    }
    drop(busy);
    let _ = app.emit("state-changed", ());
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

const AUTOSTART_PORTABLE: &str = "Автозапуск работает только у установленной версии Prism. Портативную копию может подменить любая программа, а задание запускало бы её с правами администратора без вопроса.";

fn set_autostart_task(on: bool, dir: &std::path::Path, log: &Log) -> R<()> {
    if !on {
        run_quiet("schtasks", &["/Delete", "/F", "/TN", TASK_NAME]);
        log.push("[prism] автозапуск выключен");
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if !secure::in_program_files(&exe) {
        return Err(AUTOSTART_PORTABLE.into());
    }
    // schtasks читает XML в UTF-16 с меткой порядка байтов, как его сохраняет сам планировщик.
    let mut bytes = vec![0xFF, 0xFE];
    for unit in task_xml(&exe).encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let file = dir.join("prism-autostart.xml");
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
    set_autostart_task(on, &p.data, &p.log)?;
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
        {
            let mut i = p.inner.lock().unwrap();
            i.engine.stop();
            i.proxy.stop();
        }
        engine::kill_stray(&p.zapret);
        engine::unload_driver(&p.zapret);
    }
}

/// Сторож движка. Если winws.exe упал, пока Prism свёрнут в трей, обход раньше молча
/// пропадал до следующего запуска. Теперь он перезапускается, но не чаще
/// MAX_RESTARTS раз за 5 минут: если стратегия падает стабильно, обход выключается
/// и окно говорит об этом, чтобы не крутиться в цикле.
async fn watchdog(app: AppHandle) {
    let p = app.state::<Prism>();
    let mut restarts: Vec<Instant> = Vec::new();
    // Прошлый перезапуск не удался: процесса движка нет, а обход должен работать.
    // Без этого флажка сторож больше не замечал бы поломку: exited() без процесса молчит.
    let mut pending = false;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let code = {
            let mut i = p.inner.lock().unwrap();
            if i.busy.is_some() {
                None
            } else if !i.settings.engine_on {
                pending = false;
                None
            } else if let Some(c) = i.engine.exited() {
                Some(c)
            } else if pending && !i.engine.alive() {
                Some("прошлый перезапуск не удался".to_string())
            } else {
                pending = false;
                None
            }
        };
        let Some(code) = code else { continue };
        let Ok(busy) = p.begin(RESTARTING) else { continue };
        restarts.retain(|t| t.elapsed() < Duration::from_secs(300));
        if restarts.len() >= MAX_RESTARTS {
            stop_engine(&p);
            {
                let mut i = p.inner.lock().unwrap();
                i.settings.engine_on = false;
                p.save(&i);
            }
            restarts.clear();
            pending = false;
            drop(busy);
            let text = "Обход выключен: winws.exe падает снова и снова. Нажмите «Подобрать» или выберите другую стратегию";
            p.log.push(format!("[prism] winws.exe завершился ({code}) {MAX_RESTARTS} раза за 5 минут. {text}"));
            let _ = app.emit("notice", text);
            let _ = app.emit("state-changed", ());
            continue;
        }
        restarts.push(Instant::now());
        p.log.push(format!("[prism] winws.exe неожиданно завершился ({code}), перезапускаю"));
        match start_engine(&p).await {
            Ok(()) => {
                pending = false;
                p.log.push("[prism] обход снова работает");
            }
            Err(e) => {
                pending = true;
                p.log.push(format!("[prism] {e}"));
            }
        }
        drop(busy);
        let _ = app.emit("state-changed", ());
    }
}

/// Следить за сетью в отдельном потоке. Имя сети узнаётся после каждой перемены
/// адресов и ещё трижды с паузой в 10 секунд: Windows не сразу опознаёт новую сеть.
/// Без перемен сеть проверяется раз в 3 минуты: так видны и перемены только в IPv6.
fn watch_network(app: AppHandle) {
    let changes = net::changes();
    // При запуске вместе с Windows сеть часто ещё не опознана.
    let mut quick = 3;
    loop {
        let name = net::current().unwrap_or_default();
        let p = app.state::<Prism>();
        let gen = {
            let mut cur = p.network.lock().unwrap();
            if *cur == name {
                None
            } else {
                *cur = name.clone();
                Some(p.net_gen.fetch_add(1, Ordering::SeqCst) + 1)
            }
        };
        if let Some(gen) = gen {
            if !name.is_empty() {
                p.log.push(format!("[prism] сеть: «{name}»"));
            }
            tauri::async_runtime::spawn(apply_profile(app.clone(), gen));
        }
        let wait = if quick > 0 {
            quick -= 1;
            Duration::from_secs(10)
        } else {
            Duration::from_secs(180)
        };
        match changes.recv_timeout(wait) {
            Ok(()) => {
                // Адреса меняются пачкой (IPv4, IPv6, DNS): ждём, пока сеть устоится.
                std::thread::sleep(Duration::from_secs(4));
                while changes.try_recv().is_ok() {}
                quick = 3;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(wait),
        }
    }
}

/// Сообщить о том, из-за чего Prism не может работать, и закрыться.
/// Без этого программа молча исчезала: окна ещё нет, а ошибка уходила в никуда.
fn fatal(text: &str) -> ! {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND};
    let w = |s: &str| std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let body = format!("{text}\n\nЕсли ошибка повторяется, напишите о ней в https://github.com/{}/issues", updates::APP_REPO);
    unsafe { MessageBoxW(std::ptr::null_mut(), w(&body).as_ptr(), w("Prism не запустился").as_ptr(), MB_OK | MB_ICONERROR | MB_SETFOREGROUND) };
    std::process::exit(1)
}

/// Свои списки сайтов и подсетей, которые переносятся из прошлых версий.
const USER_LISTS: [&str; 3] = ["list-general-user.txt", "list-exclude-user.txt", "ipset-exclude-user.txt"];

/// Строка списка zapret: домен, IP или подсеть. Остальное при переносе отбрасывается.
fn list_line(l: &str) -> bool {
    !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || ".-:/_".contains(c))
}

/// Прошлые версии хранили настройки и списки в %LOCALAPPDATA%, куда может писать
/// любая программа пользователя. Переносим их один раз: файлы читаются без перехода
/// по ссылкам, из списков берутся только адреса, а настройки проверяются при запуске.
fn migrate_old(old: &Path, zapret: &ZapretPaths, log: &Log) -> Settings {
    let Some(text) = secure::read_user_file(&old.join("settings.json"), 64 * 1024) else {
        return Settings::default();
    };
    let settings = Settings::parse(&text);
    let _ = std::fs::create_dir_all(zapret.lists());
    for name in USER_LISTS {
        let Some(body) = secure::read_user_file(&old.join("zapret").join("lists").join(name), 1 << 20) else { continue };
        let lines: Vec<&str> = body.lines().map(str::trim).filter(|l| list_line(l)).collect();
        if !lines.is_empty() {
            let _ = std::fs::write(zapret.lists().join(name), lines.join("\r\n") + "\r\n");
        }
    }
    log.push("[prism] настройки и списки перенесены из папки прошлой версии");
    settings
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let log = Log::default();
    let res = app.path().resource_dir()?.join("resources");
    let installed = secure::in_program_files(&std::env::current_exe()?);
    let root = secure::data_dir(&log)?;
    // Файлы портативной копии может подменить пользователь, поэтому она хранит
    // всё отдельно и не подсовывает свои файлы установленной версии.
    let data = if installed { root } else { root.join("portable") };
    std::fs::create_dir_all(&data)?;

    let zapret = ZapretPaths { root: data.join("zapret") };
    if let Err(e) = engine::install(&res.join("zapret"), &zapret.root, &log) {
        log.push(format!("[prism] {e}"));
    }
    // Программы, которые мешают zapret, Prism только называет в журнале.
    let diag = log.clone();
    std::thread::spawn(move || engine::diagnose(&diag));
    let settings_file = data.join("settings.json");
    let mut settings = if settings_file.exists() {
        Settings::load(&settings_file)
    } else {
        migrate_old(&app.path().app_local_data_dir()?, &zapret, &log)
    };
    if !tgproxy::valid_secret(&settings.tg_secret) {
        settings.tg_secret = tgproxy::new_secret();
    }
    if !engine::list_strategies(&zapret).iter().any(|s| s.id == settings.strategy) {
        settings.strategy = "general.bat".into();
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
    let mica_ok = windows_build() >= 22000;
    let mica = mica_ok && settings.mica;

    app.manage(Prism {
        inner: Mutex::new(Inner { settings, engine: Engine::default(), proxy: Proxy::default(), busy: None }),
        log,
        zapret,
        tg_exe: res.join("tgproxy").join("tg-ws-proxy.exe"),
        tg_version: tg_version(&res),
        settings_file,
        hosts_cache: data.join("ai-hosts.txt"),
        data,
        dns_note: Mutex::new(""),
        cancel: AtomicBool::new(false),
        network: Mutex::new(String::new()),
        net_gen: AtomicU64::new(0),
        hosts_lock: Mutex::new(()),
        mica_ok,
    });

    if mica {
        apply_mica(app.handle(), true);
    }

    // Трей: быстрые действия без окна. Значок серый, пока обход выключен.
    let base = app.default_window_icon().ok_or("no icon")?;
    let (w, h) = (base.width(), base.height());
    let icon_on = Image::new_owned(base.rgba().to_vec(), w, h);
    let icon_off = Image::new_owned(gray(base.rgba()), w, h);
    let show = MenuItem::with_id(app, "show", "Открыть Prism", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Включить обход", true, None::<&str>)?;
    let pick = MenuItem::with_id(app, "pick", "Подобрать стратегию", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Выйти", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &toggle, &pick, &sep, &quit])?;
    TrayIconBuilder::with_id("main")
        .icon(icon_off.clone())
        .tooltip("Prism")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "show" => show_main(app),
            "toggle" => {
                tauri::async_runtime::spawn(tray_toggle(app.clone()));
            }
            "pick" => {
                show_main(app);
                let _ = app.emit("tray-pick", ());
            }
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

    // Значок, подсказка и пункт меню в трее следят за обходом, даже когда окно скрыто.
    let tip = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let mut last: Option<bool> = None;
        loop {
            let on = tip.state::<Prism>().inner.lock().unwrap().engine.alive();
            if last != Some(on) {
                if let Some(t) = tip.tray_by_id("main") {
                    let text = if on { "Prism: обход включён" } else { "Prism: обход выключен" };
                    let _ = t.set_icon(Some(if on { icon_on.clone() } else { icon_off.clone() }));
                    let _ = toggle.set_text(if on { "Выключить обход" } else { "Включить обход" });
                    if t.set_tooltip(Some(text)).is_ok() {
                        last = Some(on);
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    tauri::async_runtime::spawn(watchdog(app.handle().clone()));
    let net_app = app.handle().clone();
    std::thread::spawn(move || watch_network(net_app));

    // Вернуть то, что было включено в прошлый раз.
    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let p = handle.state::<Prism>();
        if restore.0 {
            // Ждём движок, а не пропускаем запуск: профиль сети мог занять его первым
            // и только выбрать стратегию, ведь обход ещё не работал.
            let _busy = p.wait_begin(RESTARTING).await;
            let running = p.inner.lock().unwrap().engine.alive();
            if !running {
                if let Err(e) = start_engine(&p).await {
                    p.log.push(format!("[prism] {e}"));
                }
            }
        }
        if restore.1 {
            let mut i = p.inner.lock().unwrap();
            let (port, secret) = (i.settings.tg_port, i.settings.tg_secret.clone());
            if let Err(e) = i.proxy.start(&p.tg_exe, port, &secret, &p.log) {
                p.log.push(format!("[prism] {e}"));
            }
        }
        // Блок адресов нейросетей мог пропасть из hosts: его убирает деинсталлятор
        // при переустановке, а иногда антивирус. Возвращаем его из сохранённой копии.
        let ai = p.inner.lock().unwrap().settings.ai;
        if ai && !hosts::present() {
            match std::fs::read_to_string(&p.hosts_cache) {
                Ok(list) => match apply_ai_hosts(&p, &list) {
                    Ok(true) => p.log.push("[prism] адреса нейросетей снова записаны в hosts"),
                    Ok(false) => {}
                    Err(e) => p.log.push(format!("[prism] {e}")),
                },
                Err(_) => {
                    if let Err(e) = refresh_ai_hosts(&p).await {
                        p.log.push(format!("[prism] {e}"));
                    }
                }
            }
        }
        let _ = handle.emit("state-changed", ());

        // Обновить задание автозапуска: путь к программе мог измениться,
        // а задания из старых версий не запускались от батареи.
        // Портативным копиям старые версии тоже ставили задание, такое задание убираем.
        if keep_task {
            if installed {
                if let Err(e) = set_autostart_task(true, &p.data, &p.log) {
                    p.log.push(format!("[prism] {e}"));
                }
            } else {
                let _ = set_autostart_task(false, &p.data, &p.log);
                let mut i = p.inner.lock().unwrap();
                i.settings.autostart = false;
                p.save(&i);
                p.log.push(format!("[prism] {AUTOSTART_PORTABLE}"));
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
    // Деинсталлятор: убрать то, что Prism менял в Windows, и выйти, не открывая окно.
    // Проверяем до запуска Tauri: иначе плагин одного экземпляра просто показал бы
    // уже открытый Prism, и уборка не случилась бы. Код выхода 2: убрать удалось не всё.
    let args: Vec<String> = std::env::args().collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    if flag("--cleanup") {
        let ok = cleanup::run(flag("--purge"), flag("--quiet"));
        std::process::exit(if ok { 0 } else { 2 });
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .setup(|app| {
            if let Err(e) = setup(app) {
                fatal(&e.to_string());
            }
            Ok(())
        })
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
            add_site, remove_site, check_services, auto_pick, stop_pick, set_autostart, set_tray, get_log, close_window,
            check_updates, update_zapret, update_app, set_auto_update, set_mica
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|e| fatal(&format!("{e}. Возможно, не установлен Microsoft Edge WebView2.")));

    app.run(|app, e| {
        if let RunEvent::Exit = e {
            shutdown(app);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_addresses() {
        assert!(list_line("rutracker.org"));
        assert!(list_line("203.0.113.113/32"));
        assert!(list_line("2a00:1450::/32"));
        assert!(!list_line("# Never leave this file empty"));
        assert!(!list_line("--debug=@C:\\x"));
        assert!(!list_line(""));
    }

    #[test]
    fn reads_windows_build() {
        assert_eq!(parse_build("\r\nMicrosoft Windows [Version 10.0.22631.4602]\r\n"), 22631);
        assert_eq!(parse_build("Microsoft Windows [Версия 10.0.19045.3803]"), 19045);
        assert_eq!(parse_build(""), 0);
        assert!(windows_build() > 0);
    }

    #[test]
    fn grays_icon() {
        assert_eq!(gray(&[255, 0, 0, 255, 10, 20, 30, 0]), vec![76, 76, 76, 140, 18, 18, 18, 0]);
    }
}
