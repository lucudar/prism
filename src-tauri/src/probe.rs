//! Проверка, открываются ли сервисы.

use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Clone, Serialize, Default)]
pub struct Check {
    pub ok: bool,
    pub ms: u64,
    /// Сколько сайтов из проверенных ответили. Для YouTube это важно:
    /// страница может открываться, а видео с googlevideo.com — нет.
    pub good: u8,
    pub total: u8,
}

fn builder(timeout: u64) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .connect_timeout(Duration::from_secs(timeout))
        // Без этого Google отдаёт 429 и редирект на страницу согласия.
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36")
        .redirect(reqwest::redirect::Policy::limited(3))
        .no_proxy()
}

pub fn client(timeout: u64) -> reqwest::Client {
    builder(timeout).build().expect("http client")
}

/// Коротко, почему адрес не открылся: для журнала.
fn reason(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "нет ответа".into();
    }
    // Понятнее всего самая глубокая причина: сброс соединения, ошибка сертификата и т. п.
    let mut src: &dyn Error = e;
    while let Some(s) = src.source() {
        src = s;
    }
    let text = src.to_string();
    let text = text.trim();
    if text.chars().count() > 120 {
        format!("{}…", text.chars().take(120).collect::<String>())
    } else {
        text.to_string()
    }
}

/// Открыть адрес и прочитать ответ. Возвращает, за сколько миллисекунд он открылся.
async fn fetch(c: &reqwest::Client, url: &str) -> Result<u64, String> {
    let t = Instant::now();
    let r = c.get(url).send().await.map_err(|e| reason(&e))?;
    // Читаем и тело: DPI часто рвёт соединение уже после заголовков.
    r.bytes().await.map_err(|e| reason(&e))?;
    Ok(t.elapsed().as_millis() as u64)
}

/// Один адрес отвечает, если получили любой HTTP-ответ: DPI чаще режет
/// соединение или молчит, чем отдаёт ошибку по существу.
pub async fn http(c: &reqwest::Client, url: &str) -> Check {
    match fetch(c, url).await {
        Ok(ms) => Check { ok: true, ms, ..Default::default() },
        Err(_) => Check::default(),
    }
}

/// Сервис открывается, если ответил хотя бы один из адресов.
/// Для YouTube проверяем и страницу, и сам видеосервер: на части провайдеров
/// сайт открывается, а видео не грузится.
pub async fn any(c: &reqwest::Client, urls: &[&str]) -> Check {
    let mut good = 0u8;
    let mut best = 0u64;
    for u in urls {
        let r = http(c, u).await;
        if r.ok {
            good += 1;
            best = if best == 0 { r.ms } else { best.min(r.ms) };
        }
    }
    let total = urls.len() as u8;
    Check { ok: good > 0, ms: if good > 0 { best } else { 0 }, good, total }
}

pub async fn tcp(port: u16) -> Check {
    let t = Instant::now();
    let r = tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(("127.0.0.1", port))).await;
    Check { ok: matches!(r, Ok(Ok(_))), ms: t.elapsed().as_millis() as u64, ..Default::default() }
}

/// Адреса YouTube: название для журнала, сайт и путь. Страница сайта, видеосервер
/// и api, через которое плеер получает данные о видео.
const YOUTUBE: [(&str, &str, &str); 3] = [
    ("сайт", "www.youtube.com", "/generate_204"),
    ("видеосервер", "redirector.googlevideo.com", "/report_mapping?di=no"),
    ("api", "youtubei.googleapis.com", "/youtubei/v1/visitor_id"),
];

/// Адреса YouTube от безопасного DNS (DNS-over-HTTPS у Google и Cloudflare).
/// DNS провайдера может отвечать на www.youtube.com адресом, который не открывается,
/// и тогда не помогает ни одна стратегия: браузер пишет «Нет подключения к интернету»
/// (Flowseal, issue #8431). По этим адресам видно, работает ли сам обход.
#[derive(Clone, Default)]
pub struct Dns(Vec<(&'static str, Vec<IpAddr>)>);

/// Последний ответ безопасного DNS и когда он получен.
static DNS_CACHE: Mutex<Option<(Instant, Dns)>> = Mutex::new(None);

impl Dns {
    /// Спросить адреса YouTube у безопасного DNS.
    pub async fn lookup() -> Dns {
        let c = doh_client();
        let [a, b, d] = YOUTUBE.map(|(_, host, _)| host);
        let (ia, ib, id) = tokio::join!(doh(&c, a), doh(&c, b), doh(&c, d));
        let dns = Dns([(a, ia), (b, ib), (d, id)].into_iter().filter(|(_, ips)| !ips.is_empty()).collect());
        *DNS_CACHE.lock().unwrap() = Some((Instant::now(), dns.clone()));
        dns
    }

    /// Последний ответ, если он свежий: проверка на главной идёт каждую минуту,
    /// а спрашивать DNS так часто незачем.
    pub async fn cached() -> Dns {
        let fresh = {
            let cache = DNS_CACHE.lock().unwrap();
            match &*cache {
                // Пустой ответ спрашиваем снова через 2 минуты, удачный — через 10.
                Some((at, dns)) if at.elapsed() < Duration::from_secs(if dns.is_empty() { 120 } else { 600 }) => Some(dns.clone()),
                _ => None,
            }
        };
        match fresh {
            Some(dns) => dns,
            None => Dns::lookup().await,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Адреса сайта от безопасного DNS.
    pub fn ips(&self, host: &str) -> &[IpAddr] {
        self.0.iter().find(|(h, _)| *h == host).map(|(_, ips)| ips.as_slice()).unwrap_or(&[])
    }

    /// Клиент, который подключается к YouTube по адресам от безопасного DNS.
    /// Остальные сайты он открывает как обычно.
    pub fn client(&self, timeout: u64) -> reqwest::Client {
        let mut b = builder(timeout);
        for (host, ips) in &self.0 {
            let addrs: Vec<SocketAddr> = ips.iter().map(|ip| SocketAddr::new(*ip, 443)).collect();
            b = b.resolve_to_addrs(host, &addrs);
        }
        b.build().expect("http client")
    }
}

/// Клиент для DNS-over-HTTPS. Адреса самих серверов DNS заданы заранее,
/// чтобы не спрашивать о них DNS провайдера.
fn doh_client() -> reqwest::Client {
    let v4 = |a: u8, b: u8, c: u8, d: u8| SocketAddr::new(IpAddr::V4(Ipv4Addr::new(a, b, c, d)), 443);
    builder(3)
        .resolve_to_addrs("dns.google", &[v4(8, 8, 8, 8), v4(8, 8, 4, 4)])
        .resolve_to_addrs("cloudflare-dns.com", &[v4(1, 1, 1, 1), v4(1, 0, 0, 1)])
        .build()
        .expect("http client")
}

/// Адреса IPv4 сайта у Google Public DNS, а если он не ответил — у Cloudflare.
async fn doh(c: &reqwest::Client, host: &str) -> Vec<IpAddr> {
    for base in ["https://dns.google/resolve", "https://cloudflare-dns.com/dns-query"] {
        let url = format!("{base}?name={host}&type=A");
        let r = match c.get(&url).header("accept", "application/dns-json").send().await {
            Ok(r) => r,
            Err(_) => continue,
        };
        if let Ok(v) = r.json::<serde_json::Value>().await {
            let ips = parse_doh(&v);
            if !ips.is_empty() {
                return ips;
            }
        }
    }
    Vec::new()
}

/// Адреса IPv4 из ответа DNS-over-HTTPS в формате JSON.
fn parse_doh(v: &serde_json::Value) -> Vec<IpAddr> {
    let mut out = Vec::new();
    for a in v["Answer"].as_array().into_iter().flatten() {
        if a["type"].as_u64() != Some(1) {
            continue;
        }
        if let Some(ip) = a["data"].as_str().and_then(|s| s.parse::<IpAddr>().ok()) {
            if !out.contains(&ip) {
                out.push(ip);
            }
        }
    }
    out
}

/// Адреса сайта от DNS, который настроен в Windows: обычно это DNS провайдера
/// или роутера. Его же по умолчанию спрашивает браузер.
async fn system_ips(host: &str) -> Vec<IpAddr> {
    let mut out = Vec::new();
    let found = tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host((host, 443u16))).await;
    if let Ok(Ok(addrs)) = found {
        for a in addrs {
            if !out.contains(&a.ip()) {
                out.push(a.ip());
            }
        }
    }
    out
}

pub struct Services {
    pub discord: Check,
    pub youtube: Check,
}

/// Проверить Discord и YouTube. YouTube проверяется по адресам от безопасного DNS,
/// если они есть: так видно, работает ли обход, даже когда DNS провайдера
/// выдаёт нерабочий адрес.
pub async fn services(timeout: u64, dns: &Dns) -> Services {
    let (c, yt) = (client(timeout), dns.client(timeout));
    let (discord, youtube) = tokio::join!(discord_checks(&c), youtube_checks(&yt));
    Services { discord, youtube }
}

pub async fn discord_checks(c: &reqwest::Client) -> Check {
    any(c, &[
        "https://discord.com/api/v9/experiments",
        "https://gateway.discord.gg/?v=10&encoding=json",
    ])
    .await
}

pub async fn youtube_checks(c: &reqwest::Client) -> Check {
    let owned = YOUTUBE.map(|(_, host, path)| format!("https://{host}{path}"));
    let urls: Vec<&str> = owned.iter().map(String::as_str).collect();
    any(c, &urls).await
}

/// Какие адреса YouTube не открылись и почему. Нужно для журнала: видно,
/// что именно режет провайдер — сайт, видеосерверы или api.
pub async fn youtube_failed(c: &reqwest::Client) -> Vec<String> {
    let mut out = Vec::new();
    for (name, host, path) in YOUTUBE {
        if let Err(e) = fetch(c, &format!("https://{host}{path}")).await {
            out.push(format!("{name} ({e})"));
        }
    }
    out
}

/// Итог сравнения DNS Windows с безопасным DNS для www.youtube.com.
pub struct DnsCheck {
    /// Строки для журнала.
    pub lines: Vec<String>,
    /// ok — адрес от DNS Windows рабочий; dns — рабочий только адрес от безопасного DNS;
    /// blocked — не открываются оба; unknown — безопасный DNS не ответил.
    pub kind: &'static str,
    /// Браузеру нужен безопасный DNS.
    pub bad: bool,
}

/// До трёх адресов через запятую.
fn show(ips: &[IpAddr]) -> String {
    if ips.is_empty() {
        return "нет ответа".into();
    }
    let mut s = ips.iter().take(3).map(IpAddr::to_string).collect::<Vec<_>>().join(", ");
    if ips.len() > 3 {
        s.push_str(", …");
    }
    s
}

/// Открыть www.youtube.com, подключаясь к заданному адресу, а не к ответу DNS.
async fn via(ip: Option<IpAddr>, timeout: u64) -> Option<Result<u64, String>> {
    let (_, host, path) = YOUTUBE[0];
    let ip = ip?;
    let c = match builder(timeout).resolve(host, SocketAddr::new(ip, 443)).build() {
        Ok(c) => c,
        Err(e) => return Some(Err(e.to_string())),
    };
    Some(fetch(&c, &format!("https://{host}{path}")).await)
}

/// Сравнить, открывается ли www.youtube.com по адресу от DNS Windows и по адресу
/// от безопасного DNS. Если открывается только второй, обход работает, но браузер
/// с DNS провайдера идёт по нерабочему адресу и пишет «Нет подключения к интернету».
pub async fn youtube_dns(dns: &Dns, timeout: u64) -> DnsCheck {
    let (_, host, _) = YOUTUBE[0];
    let system = system_ips(host).await;
    let secure = dns.ips(host);
    // С DNS Windows браузер обычно подключается по первому адресу IPv4.
    let sys_ip = system.iter().find(|ip| ip.is_ipv4()).or(system.first()).copied();
    // Адрес от безопасного DNS, которого нет в ответе DNS Windows: так сравнение честнее.
    let sec_ip = secure.iter().find(|ip| !system.contains(ip)).or(secure.first()).copied();
    let (sys, sec) = tokio::join!(via(sys_ip, timeout), via(sec_ip, timeout));
    let res = |ip: Option<IpAddr>, r: &Option<Result<u64, String>>| match (ip, r) {
        (Some(ip), Some(Ok(ms))) => format!("{ip} открывается, {ms} мс"),
        (Some(ip), Some(Err(e))) => format!("{ip} не открывается ({e})"),
        _ => "адреса нет".to_string(),
    };
    let mut lines = vec![
        format!("[prism] адрес {host}: системный DNS — {}; безопасный DNS — {}", show(&system), show(secure)),
        format!("[prism] {host} по адресу системного DNS: {}; по адресу безопасного DNS: {}", res(sys_ip, &sys), res(sec_ip, &sec)),
    ];
    let works = |r: &Option<Result<u64, String>>| matches!(r, Some(Ok(_)));
    let (kind, bad, verdict) = if works(&sys) {
        ("ok", false, "DNS в порядке: адрес YouTube от DNS Windows рабочий")
    } else if works(&sec) {
        ("dns", true, "YouTube открывается только по адресу от безопасного DNS, поэтому браузер пишет «Нет подключения к интернету». Включите в браузере безопасный DNS (Google или Cloudflare). В Яндекс Браузере это не помогает: откройте YouTube в Chrome, Edge или Firefox")
    } else if sec_ip.is_some() {
        ("blocked", false, "YouTube не открывается ни по адресу от DNS Windows, ни по адресу от безопасного DNS: дело не в DNS, нажмите «Подобрать»")
    } else {
        ("unknown", false, "безопасный DNS не ответил, сравнить адреса YouTube не получилось")
    };
    lines.push(format!("[prism] {verdict}"));
    DnsCheck { lines, kind, bad }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_partial() {
        let c = Check { ok: true, ms: 10, good: 2, total: 3 };
        assert!(c.ok && c.good < c.total);
    }

    #[test]
    fn parses_doh_answer() {
        let v = serde_json::json!({
            "Status": 0,
            "Answer": [
                { "name": "www.youtube.com.", "type": 5, "data": "youtube-ui.l.google.com." },
                { "name": "youtube-ui.l.google.com.", "type": 1, "data": "142.250.74.14" },
                { "name": "youtube-ui.l.google.com.", "type": 1, "data": "142.250.74.14" },
                { "name": "youtube-ui.l.google.com.", "type": 1, "data": "172.217.21.142" }
            ]
        });
        let ips: Vec<String> = parse_doh(&v).iter().map(IpAddr::to_string).collect();
        assert_eq!(ips, vec!["142.250.74.14", "172.217.21.142"]);
        assert!(parse_doh(&serde_json::json!({ "Status": 3 })).is_empty());
        assert_eq!(show(&[]), "нет ответа");
    }
}
