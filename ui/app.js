'use strict';
const T = window.__TAURI__;
const $ = s => document.querySelector(s), $$ = s => [...document.querySelectorAll(s)];

// В обычном браузере (предпросмотр) работает имитация, в программе — настоящие команды.
const mock = (() => {
  const st = { engine_on:false, strategy:'general.bat', games:false, ai:false, tg_on:false, autostart:false, tray:true,
    tg:{host:'127.0.0.1',port:1443,secret:'7f3a9c41e0b25d88f6c1a04e93b7d2aa',link:'tg://proxy?server=127.0.0.1&port=1443&secret=dd7f3a9c41e0b25d88f6c1a04e93b7d2aa'},
    strategies:[['general.bat','Основная','Рекомендуемая, начните с неё'],['general (ALT).bat','ALT','Альтернативная разрезка пакетов'],['general (ALT2).bat','ALT2','Альтернативная разрезка пакетов'],['general (FAKE TLS AUTO).bat','FAKE TLS AUTO','Поддельное TLS-приветствие, часто помогает на мобильном интернете'],['general (SIMPLE FAKE).bat','SIMPLE FAKE','Простые фейковые пакеты']].map(([id,name,desc])=>({id,name,desc})),
    since:0, user_sites:['rutracker.org','nnmclub.to'], builtin_sites:142, auto_update:true, app_version:'0.14.0', zapret_version:'1.10.4', portable:false, versions:['zapret-discord-youtube 1.10.4 (winws из zapret v72.9)','tg-ws-proxy 1.11.1'],
    network:'Home-WiFi', profile:true, mica:false, mica_ok:false };
  const wait = ms => new Promise(r => setTimeout(r, ms));
  let stopFlag = false;
  return async (cmd, a = {}) => {
    await wait(300);
    switch (cmd) {
      case 'set_engine': await wait(a.on ? 1200 : 300); st.engine_on = a.on; st.since = a.on ? Date.now() - 2 * 3600e3 - 14 * 60e3 : 0; break;
      case 'set_strategy': st.strategy = a.id; st.profile = true; break;
      case 'set_games': st.games = a.on; break;
      case 'set_ai': st.ai = a.on; break;
      case 'set_tg': st.tg_on = a.on; break;
      case 'set_autostart': st.autostart = a.on; break;
      case 'set_tray': st.tray = a.on; break;
      case 'set_mica': st.mica = a.on; break;
      case 'add_site': st.user_sites.unshift(a.domain); break;
      case 'remove_site': st.user_sites = st.user_sites.filter(s => s !== a.domain); break;
      case 'check_services': await wait(900); return { discord:{ok:st.engine_on,ms:120,good:2,total:2}, youtube:{ok:st.engine_on,ms:140,good:3,total:3}, telegram:{ok:st.tg_on,ms:1,good:0,total:0}, chatgpt:st.ai, youtube_dns:false };
      case 'auto_pick':
        stopFlag = false;
        for (const s of st.strategies) { if (stopFlag) break; emit('pick-progress',{id:s.id,state:'wait',ms:0}); await wait(400); emit('pick-progress',{id:s.id,state:s.id.includes('ALT2')?'bad':'ok',ms:100+Math.round(Math.random()*150)}); }
        st.strategy = 'general.bat'; st.engine_on = true; st.since = Date.now(); st.profile = true; break;
      case 'stop_pick': stopFlag = true; return null;
      case 'get_log': return '[prism] предпросмотр в браузере';
      case 'set_auto_update': st.auto_update = a.on; break;
      case 'check_updates': return { app:{current:'0.14.0',latest:'0.14.1',available:true}, zapret:{current:'1.10.4',latest:'1.10.4',available:false} };
      case 'update_zapret': throw 'У вас уже последняя версия обходов';
      case 'update_app': return null;
      case 'open_tg_link': case 'close_window': return null;
    }
    return JSON.parse(JSON.stringify(st));
  };
})();
const handlers = {};
function emit(ev, payload) { (handlers[ev] || []).forEach(f => f({ payload })); }
const invoke = T ? T.core.invoke : mock;
const listen = T ? T.event.listen : async (ev, f) => { (handlers[ev] ||= []).push(f); };

// Движение. «Уменьшить движение» в Windows выключает анимации текста и списков.
const calm = matchMedia('(prefers-reduced-motion: reduce)');
const EASE = 'cubic-bezier(.2,.8,.2,1)';

// Окно меняется, только когда что-то правда изменилось: на каждом обновлении
// состояния браузер не пересчитывает раскладку ради того же самого текста.
function setText(el, v, anim) {
  v = String(v);
  if (el.textContent === v) return;
  el.textContent = v;
  if (anim) pop(el);
}
// Новый текст проявляется, а не подменяется рывком.
function pop(el, dy = 3) {
  if (calm.matches || !el.animate) return;
  el.animate(dy ? [{ opacity: 0, transform: `translateY(${dy}px)` }, { opacity: 1, transform: 'none' }] : [{ opacity: .35 }, { opacity: 1 }],
    { duration: 220, easing: EASE });
}
function setAttr(el, k, v) { v = String(v); if (el.getAttribute(k) !== v) el.setAttribute(k, v); }
function setHidden(el, h) { h = !!h; if (el.hidden !== h) el.hidden = h; }
function setClass(el, c) { if (el.className !== c) el.className = c; }
// 1 сайт, 2 сайта, 5 сайтов
function plural(n, one, few, many) {
  const a = n % 10, b = n % 100;
  return a === 1 && b !== 11 ? one : a >= 2 && a <= 4 && (b < 12 || b > 14) ? few : many;
}

let S = null;          // последнее состояние из программы
let svc = null;        // результаты последней проверки Discord и YouTube
let checkedAt = 0;     // когда была последняя проверка
let onSince = 0;       // запасной вариант, если программа не прислала время включения
let busy = null;       // обход включается или выключается: {text, target}
let checkGen = 0;      // поколение проверки: старые ответы после переключений не показываем
const invalidate = () => { checkGen++; svc = null; };
let picking = false;   // идёт подбор стратегии
let stopping = false;  // нажали «Стоп», подбор заканчивает текущую проверку
const pick = {};       // результаты подбора по id стратегии
const PICK_WAIT = 'Дождитесь конца подбора или нажмите «Стоп»';

const toast = t => {
  const el = $('#toast'), shown = el.classList.contains('show');
  el.textContent = t;
  el.classList.add('show');
  if (shown) pop(el, 0);
  clearTimeout(toast.t);
  toast.t = setTimeout(() => el.classList.remove('show'), 2800);
};
const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;' }[c]));
const both = a => a.length > 1 ? a.slice(0, -1).join(', ') + ' и ' + a[a.length - 1] : a[0];

async function call(cmd, args, okText) {
  try {
    const r = await invoke(cmd, args);
    if (r && typeof r === 'object' && 'strategies' in r) { S = r; render(); }
    if (okText) toast(okText);
    return r;
  } catch (e) {
    toast(String(e));
    await refresh();
    throw e;
  }
}
// Окно появляется, когда пришло состояние, а не с заглушками, которые тут же меняются.
const ready = () => document.body.classList.add('ready');
async function refresh() {
  try { S = await invoke('get_state'); render(); } finally { ready(); }
}

// Окно
$('#wMin').onclick = () => T && T.window.getCurrentWindow().minimize();
$('#wClose').onclick = () => invoke('close_window');

// Разделы: подложка выбранной вкладки переезжает пружиной, а страница
// въезжает с той стороны, где стоит вкладка.
const tabs = $$('.seg button'), inkEl = $('.seg-ink');
let page = 'home';
function ink(instant) {
  const cur = tabs.find(b => b.dataset.go === page);
  if (!cur || !inkEl) return;
  if (instant) inkEl.style.transition = 'none';
  inkEl.style.setProperty('--x', cur.offsetLeft + 'px');
  inkEl.style.setProperty('--w', cur.offsetWidth + 'px');
  if (instant) { inkEl.getBoundingClientRect(); inkEl.style.transition = ''; }
}
function go(next) {
  if (next === page) return;
  const from = tabs.findIndex(b => b.dataset.go === page), to = tabs.findIndex(b => b.dataset.go === next);
  $('main').style.setProperty('--dir', to > from ? 1 : -1);
  page = next;
  tabs.forEach(b => b.dataset.go === next ? b.setAttribute('aria-current', 'page') : b.removeAttribute('aria-current'));
  $$('.page').forEach(p => p.classList.toggle('on', p.id === 'p-' + next));
  ink();
  edge();
  if (next === 'set') loadLog();
}
// Шапка становится стеклом, только когда под неё уезжает контент.
function edge() {
  const p = $('.page.on');
  document.body.classList.toggle('scrolled', !!p && p.scrollTop > 2);
}
$$('.page').forEach(p => p.addEventListener('scroll', () => { if (p.classList.contains('on')) edge(); }, { passive: true }));
tabs.forEach(b => b.onclick = () => go(b.dataset.go));
addEventListener('resize', () => ink(true));
document.fonts && document.fonts.ready.then(() => ink(true));
ink(true);

// Главный переключатель
const powerBtn = $('#power'), dial = $('.dial');
powerBtn.addEventListener('pointerdown', e => {
  const r = powerBtn.getBoundingClientRect();
  const x = e.clientX - r.left, y = e.clientY - r.top;
  const d = 2 * Math.hypot(Math.max(x, r.width - x), Math.max(y, r.height - y));
  const el = document.createElement('span');
  el.className = 'ripple';
  el.style.cssText = `left:${x - d / 2}px;top:${y - d / 2}px;width:${d}px;height:${d}px`;
  powerBtn.appendChild(el);
  el.classList.add('go');
  setTimeout(() => el.remove(), 650);
});
// Блик на кнопке чуть тянется за курсором, как свет на стекле. Сдвигается
// только слой блика, раз в кадр, без перерисовки кнопки.
let glint = 0, gp = [0, 0];
dial.addEventListener('pointermove', e => {
  if (calm.matches) return;
  gp = [e.clientX, e.clientY];
  glint ||= requestAnimationFrame(() => {
    glint = 0;
    const r = powerBtn.getBoundingClientRect(), k = v => Math.max(-1, Math.min(1, v));
    const dx = k((gp[0] - r.left) / r.width * 2 - 1), dy = k((gp[1] - r.top) / r.height * 2 - 1);
    powerBtn.style.setProperty('--gx', (dx * 16).toFixed(1) + 'px');
    powerBtn.style.setProperty('--gy', (dy * 12).toFixed(1) + 'px');
  });
});
dial.addEventListener('pointerleave', () => {
  cancelAnimationFrame(glint); glint = 0;
  powerBtn.style.setProperty('--gx', '0px');
  powerBtn.style.setProperty('--gy', '0px');
});
powerBtn.onclick = async () => {
  if (busy || picking || !S) return;
  const on = !S.engine_on;
  busy = { text: on ? 'Включаю…' : 'Выключаю…', target: on }; invalidate(); render();
  try { await call('set_engine', { on }, on ? 'Обход включён' : 'Обход выключен'); } catch {}
  busy = null; render(); checkSoon();
};

// Остальные переключатели
const modCmd = { tg:'set_tg', games:'set_games', ai:'set_ai', autostart:'set_autostart', tray:'set_tray', auto_update:'set_auto_update', mica:'set_mica' };
const modText = {
  tg: on => on ? 'Прокси для Telegram включён' : 'Прокси для Telegram выключен',
  games: on => on ? 'Режим для игр включён' : 'Режим для игр выключен',
  ai: on => on ? 'Доступ к нейросетям включён' : 'Доступ к нейросетям выключен',
  autostart: on => on ? 'Автозапуск включён' : 'Автозапуск выключен',
  tray: on => on ? 'Крестик будет сворачивать в трей' : 'Крестик будет закрывать Prism',
  auto_update: on => on ? 'Автообновление включено' : 'Автообновление выключено',
  mica: on => on ? 'Прозрачный фон включён' : 'Прозрачный фон выключен',
};
const modVal = { tg: () => S.tg_on, games: () => S.games, ai: () => S.ai, autostart: () => S.autostart, tray: () => S.tray, auto_update: () => S.auto_update, mica: () => S.mica };
// Эти переключатели перезапускают движок и сбили бы подбор.
const engineMods = ['games'];
$$('.sw[data-mod]').forEach(sw => sw.onclick = async () => {
  if (!S) return;
  const m = sw.dataset.mod, on = !modVal[m]();
  if (picking && engineMods.includes(m)) { toast(PICK_WAIT); return; }
  sw.disabled = true; sw.setAttribute('aria-checked', on);
  if (m === 'tg' && svc) delete svc.telegram;
  try { await call(modCmd[m], { on }, modText[m](on)); } catch {}
  sw.disabled = false; render();
  if (m === 'tg') checkSoon();
});

// Telegram
$$('[data-copy]').forEach(b => b.onclick = () => { navigator.clipboard.writeText($('#' + b.dataset.copy).textContent); toast('Скопировано'); });
$('#tgCopyLink').onclick = () => { navigator.clipboard.writeText(S.tg.link); toast('Ссылка на прокси скопирована'); };
$('#tgOpen').onclick = async () => {
  try { await invoke('open_tg_link'); toast('Telegram предложит подключить прокси. Нажмите «Подключить»'); }
  catch (e) { toast(String(e)); }
};
$('#tgData').onclick = e => {
  const kv = $('#tgKv'), open = kv.hidden;
  kv.hidden = !open; e.currentTarget.setAttribute('aria-expanded', open);
  e.currentTarget.textContent = open ? 'Скрыть данные' : 'Данные для ручной настройки';
};

// Стратегии: подбор на главной, ручной выбор в отдельном окне
const dlg = $('#stratDlg');
let stratsKey = '';
$('#stratOpen').onclick = () => { stratsKey = ''; drawStrats(); dlg.showModal(); };
// Окно выбора закрывается так же плавно, как открывается.
function closeDlg() {
  if (!dlg.open || dlg.classList.contains('closing')) return;
  if (calm.matches) { dlg.close(); return; }
  dlg.classList.add('closing');
  let timer = 0;
  const done = () => {
    clearTimeout(timer);
    dlg.removeEventListener('animationend', onEnd);
    dlg.classList.remove('closing');
    dlg.close();
  };
  const onEnd = e => { if (e.target === dlg) done(); };
  dlg.addEventListener('animationend', onEnd);
  timer = setTimeout(done, 300);
}
$('#dlgClose').onclick = closeDlg;
dlg.onclick = e => { if (e.target === dlg) closeDlg(); };
dlg.addEventListener('cancel', e => { e.preventDefault(); closeDlg(); });
// Список перерисовывается, только когда поменялись стратегии, выбор или итоги подбора.
function drawStrats() {
  const key = JSON.stringify([S.strategy, S.strategies, pick]);
  if (key === stratsKey) return;
  stratsKey = key;
  $('#strats').innerHTML = S.strategies.map(s => {
    const r = pick[s.id];
    const res = !r ? '' : r.state === 'wait' ? '<span class="res wait">Проверяю…</span>'
      : r.state === 'ok' ? `<span class="res ok">${r.ms} мс</span>`
      : r.state === 'part' ? '<span class="res">Частично</span>' : '<span class="res bad">Не работает</span>';
    const sel = s.id === S.strategy;
    return `<div class="opt ${sel ? 'sel' : ''}" data-id="${esc(s.id)}" role="radio" tabindex="0" aria-checked="${sel}"><span class="radio"></span><div class="t"><b>${esc(s.name)}</b><span>${esc(s.desc)}</span></div>${res}</div>`;
  }).join('') || '<p class="dlg-sub">Стратегии не найдены. Переустановите Prism.</p>';
}
async function chooseStrat(o) {
  if (picking) return;
  const s = S.strategies.find(x => x.id === o.dataset.id);
  if (!s) return;
  invalidate(); closeDlg();
  try { await call('set_strategy', { id: s.id }, `Стратегия «${s.name}» ${S.engine_on ? 'включена' : 'выбрана'}`); } catch {}
  checkSoon();
}
$('#strats').addEventListener('click', e => { const o = e.target.closest('.opt'); if (o) chooseStrat(o); });
$('#strats').addEventListener('keydown', e => {
  if (e.key !== 'Enter' && e.key !== ' ') return;
  const o = e.target.closest('.opt');
  if (!o) return;
  e.preventDefault(); chooseStrat(o);
});
listen('pick-progress', e => { pick[e.payload.id] = e.payload; render(); });
// Кнопка «Подобрать» во время подбора становится «Стоп»: подбор допроверяет
// текущую стратегию и включает лучшую из уже проверенных.
$('#pickBtn').onclick = async () => {
  if (picking) {
    if (stopping) return;
    stopping = true; render();
    try { await invoke('stop_pick'); } catch {}
    return;
  }
  if (busy || !S) return;
  picking = true; stopping = false;
  for (const k in pick) delete pick[k];
  render();
  try {
    await call('auto_pick');
    const s = S.strategies.find(x => x.id === S.strategy);
    const name = s ? s.name : S.strategy;
    toast(stopping
      ? `Подбор остановлен. Стратегия «${name}»${S.engine_on ? ', обход включён' : ''}`
      : `Подобрана стратегия «${name}». Обход включён`);
  } catch {}
  // Стратегия, на которой нажали «Стоп», не проверена: не показываем ей «Проверяю…».
  for (const k in pick) if (pick[k].state === 'wait') delete pick[k];
  picking = false; stopping = false; invalidate(); render(); checkSoon();
};
// «Подобрать стратегию» в меню значка в трее: Prism открывает окно и сразу начинает.
listen('tray-pick', () => { go('home'); if (S && !picking && !busy) $('#pickBtn').click(); });

// Сайты: список перерисовывается, только когда он изменился. Новый сайт
// появляется сверху, убранный уезжает вправо.
let sitesKey = '', added = false;
function drawSites() {
  const list = S.user_sites, n = S.builtin_sites;
  const key = list.join('\n') + '|' + n;
  if (key === sitesKey) return;
  sitesKey = key;
  $('#sites').innerHTML = list.map(s => `<div class="row"><div class="t"><b>${esc(s)}</b></div><button class="x" data-del="${esc(s)}" aria-label="Убрать ${esc(s)}"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 6l12 12M18 6 6 18"/></svg></button></div>`).join('')
    || '<div class="empty">Здесь пока пусто. Добавьте сайт, который не открывается.</div>';
  setText($('#builtin'), `Ещё ${n} ${plural(n, 'сайт', 'сайта', 'сайтов')} уже есть во встроенном списке: Discord, YouTube и другие. Первые три сайта из вашего списка проверяет подбор.`);
  const first = $('#sites').firstElementChild;
  if (added && first && !calm.matches) first.animate([{ opacity: 0, transform: 'translateY(-6px)' }, { opacity: 1, transform: 'none' }], { duration: 260, easing: EASE });
  added = false;
}
$('#sites').addEventListener('click', e => {
  const b = e.target.closest('[data-del]');
  if (!b) return;
  const domain = b.dataset.del, row = b.closest('.row');
  b.disabled = true;
  row.classList.add('leaving');
  setTimeout(() => call('remove_site', { domain }, `${domain} убран из списка`)
    .catch(() => { row.classList.remove('leaving'); b.disabled = false; }), calm.matches ? 0 : 150);
});
$('#addForm').onsubmit = async e => {
  e.preventDefault();
  const v = $('#addInput').value.trim();
  if (!v) return;
  added = true;
  try { await call('add_site', { domain: v }, `${v} добавлен в список`); $('#addInput').value = ''; } catch { added = false; }
};

// Журнал
async function loadLog() { try { $('#log').textContent = (await invoke('get_log')) || 'Пока пусто'; } catch {} }
$('#logRefresh').onclick = loadLog;
$('#logCopy').onclick = () => { navigator.clipboard.writeText($('#log').textContent); toast('Журнал скопирован'); };

// Проверка Discord и YouTube
let checkTimer = null;
function checkSoon() { clearTimeout(checkTimer); checkTimer = setTimeout(checkServices, 700); }
async function checkServices() {
  if (picking || busy) return;
  const gen = checkGen, rb = $('#recheck');
  rb.classList.add('spin'); rb.disabled = true;
  try {
    const r = await invoke('check_services');
    if (gen === checkGen && !busy && !picking) { svc = r; checkedAt = Date.now(); }
  } catch {}
  rb.classList.remove('spin'); rb.disabled = false;
  render();
}
$('#recheck').onclick = () => { invalidate(); render(); checkServices(); };
setInterval(() => { if (!document.hidden) checkServices(); }, 60000);

// Проверка Discord и YouTube. Для YouTube важно не только «сайт открылся»,
// но и сколько адресов ответило: видео живёт на отдельном googlevideo.com.
function web(k) {
  if (!S.engine_on) return ['off', 'Выключено', 0];
  if (picking) return ['wait', 'Проверяю…', 0];
  const x = svc && svc[k];
  if (!x) return ['wait', 'Проверяю…', 0];
  if (!x.ok) return ['fail', 'Не открывается', 0];
  if (x.total > 1 && x.good < x.total) return ['part', `${x.ms} мс, ${x.good} из ${x.total}`, x.ms < 150 ? 3 : 2];
  return ['ok', `${x.ms} мс`, x.ms < 150 ? 3 : x.ms < 400 ? 2 : 1];
}
const ago = t => { const m = Math.floor((Date.now() - t) / 60000); return m < 1 ? 'только что' : m < 60 ? `${m} мин назад` : `${Math.floor(m / 60)} ч назад`; };
function dur(ms) {
  const m = Math.floor(ms / 60000), h = Math.floor(m / 60);
  return m < 1 ? 'меньше минуты' : h ? `${h} ч ${m % 60} мин` : `${m} мин`;
}
// Время работы и давность проверки обновляются сами, без перерисовки всего окна.
function drawClock() {
  if (!S) return;
  const on = S.engine_on && !busy && !picking;
  const since = S.since || onSince;
  setText($('#uptime'), on && since ? `Работает ${dur(Date.now() - since)}` : '');
  setText($('#checked'), S.engine_on && checkedAt && !picking ? ago(checkedAt) : '');
}
setInterval(drawClock, 15000);

let dash = '';         // дуга кольца во время подбора
function render() {
  if (!S) return;
  const busyText = busy && busy.text, target = busy && busy.target;
  const on = S.engine_on;
  if (on && !onSince) onSince = Date.now();
  if (!on) onSince = 0;
  const cl = document.body.classList;
  cl.toggle('on', busyText ? !!target : on);
  cl.toggle('busy', !!busyText);
  cl.toggle('pick', picking);
  cl.toggle('mica', !!(S.mica && S.mica_ok));
  const pw = $('#power');
  setAttr(pw, 'aria-checked', busyText ? !!target : on);
  pw.disabled = !!(busy || picking);

  // Состояние и подсказка
  const dc = web('discord');
  let yt = web('youtube');
  // YouTube открывается только по адресу от безопасного DNS: обход работает,
  // но браузер с DNS провайдера пишет «Нет подключения к интернету».
  const dnsBad = on && !busyText && !picking && !!(svc && svc.youtube_dns);
  if (dnsBad && yt[0] === 'ok') yt = ['part', 'Нужен безопасный DNS', yt[2]];
  const failed = [['Discord', dc], ['YouTube', yt]].filter(([, x]) => x[0] === 'fail').map(([n]) => n);
  const partial = [['Discord', dc], ['YouTube', yt]].filter(([, x]) => x[0] === 'part').map(([n]) => n);
  const sub = $('#stateSub');
  const total = S.strategies.length, done = Object.values(pick).filter(r => r.state !== 'wait').length;
  setText($('#state'), busyText || (picking ? (stopping ? 'Завершаю подбор' : 'Подбираю стратегию') : on ? 'Обход включён' : 'Обход выключен'), true);
  setClass(sub, 'state-sub' + ((failed.length || partial.length || dnsBad) && !busyText && !picking ? ' bad' : ''));
  setText(sub, busyText ? 'Это займёт пару секунд.'
    : picking && stopping ? 'Допроверяю текущую стратегию и включу лучшую из проверенных.'
    : picking ? `Проверяю ${Math.min(done + 1, total)} из ${total}. Это займёт минуту-другую.`
    : !on ? 'Нажмите, чтобы открылись Discord и YouTube.'
    : failed.length ? `${both(failed)} не ${failed.length > 1 ? 'открываются' : 'открывается'}. Нажмите «Подобрать», чтобы найти рабочую стратегию.`
    : dnsBad ? 'YouTube откроется только с безопасным DNS. Включите его в браузере: Настройки → Безопасность → «Использовать безопасный DNS».'
    : partial.length ? `${both(partial)}: открывается не всё. Нажмите «Подобрать», чтобы проверить другие стратегии.`
    : dc[0] === 'wait' || yt[0] === 'wait' ? 'Проверяю Discord и YouTube…'
    : 'Discord и YouTube открываются.', true);
  for (const [id, x] of [['dc', dc], ['yt', yt]]) {
    setClass($(`#svc-${id}`), 'row svc ' + x[0]);
    setText($(`#st-${id}`), x[1], true);
  }
  setHidden($('#recheck'), !on || busyText || picking);

  // Стратегия и подбор
  const strat = S.strategies.find(x => x.id === S.strategy);
  const cur = S.strategies.find(x => pick[x.id] && pick[x.id].state === 'wait');
  // Во время подбора подпись говорит, что идёт проверка, а в строке только имя:
  // так оно влезает в одну строку и строка не меняет высоту.
  setText($('#stratK'), picking ? (cur ? 'Проверяю' : 'Подбор') : 'Стратегия');
  const sName = !total ? 'Не найдены. Переустановите Prism'
    : picking ? (cur ? cur.name : 'Готовлю проверку') : (strat ? strat.name : S.strategy);
  setText($('#stratName'), sName, true);
  setAttr($('#stratName'), 'title', sName);
  $('#stratRow').classList.toggle('picking', !!picking);
  // Сеть, для которой запомнена стратегия: в другой сети Prism включит свою.
  const sn = $('#stratNet');
  setHidden(sn, !S.network);
  setText(sn, picking ? `Запомню для сети «${S.network}»` : S.profile ? `Запомнена для сети «${S.network}»` : `Сеть «${S.network}»`);
  const d = picking ? `${Math.max(2, Math.round(done / Math.max(total, 1) * 100))} 100` : '';
  if (d !== dash) { dash = d; $('#arc').style.strokeDasharray = d; }
  const pb = $('#pickBtn');
  setText(pb, picking ? 'Стоп' : 'Подобрать');
  setClass(pb, 'btn' + (failed.length && !picking ? ' primary' : ''));
  pb.disabled = picking ? stopping : !!(busy || !total);
  $('#stratOpen').disabled = picking || !total;
  $('#zapUpd').disabled = picking;
  if (dlg.open) drawStrats();

  // Модули
  for (const m in modVal) $$(`.sw[data-mod="${m}"]`).forEach(s => { if (!s.disabled) setAttr(s, 'aria-checked', modVal[m]()); });
  const tgBad = S.tg_on && svc && svc.telegram && !svc.telegram.ok;
  setText($('#tgSub'), !S.tg_on ? 'Прокси на вашем компьютере'
    : tgBad ? 'Не запустился. Подробности в журнале в настройках' : `Работает на ${S.tg.host}:${S.tg.port}`, true);
  setHidden($('#tgMore'), !S.tg_on || tgBad);
  if (!S.tg_on) setHidden($('#tgKv'), true);
  setText($('#tgHost'), S.tg.host); setText($('#tgPort'), S.tg.port); setText($('#tgSecret'), 'dd' + S.tg.secret);
  setText($('#gmSub'), !S.games ? 'Включайте, если игра тормозит или не заходит'
    : on ? 'Работает. Если игра не заходит, выключите' : 'Включится вместе с обходом', true);
  $('#tgItem').classList.toggle('is-on', !!(S.tg_on && !tgBad));
  $('#gmItem').classList.toggle('is-on', S.games);
  $('#aiItem').classList.toggle('is-on', S.ai);
  $('#asRow').classList.toggle('is-on', S.autostart);
  $$('.asSub').forEach(el => setText(el, S.autostart ? 'Включится сам при запуске Windows' : 'Включать обход при запуске Windows', true));
  setText($('#aiSub'), S.ai ? 'Работают без VPN, даже когда Prism закрыт'
    : 'ChatGPT, Gemini, Claude и другие без VPN', true);
  // Прозрачный фон есть только в Windows 11.
  setHidden($('#lookLabel'), !S.mica_ok);
  setHidden($('#lookPanel'), !S.mica_ok);

  drawUpdates();
  drawSites();
  drawClock();
}

// Обновления
let U = null;
function drawUpdates() {
  if (!S) return;
  setText($('#appVer'), `Версия ${S.app_version}` + (U && U.app.available ? `. Доступна ${U.app.latest}` : U ? '. Последняя версия' : ''));
  setText($('#zapVer'), `Версия ${S.zapret_version || 'неизвестна'}` + (U && U.zapret.available ? `. Доступна ${U.zapret.latest}` : U ? '. Последняя версия' : ''));
  setText($('#appUpd'), U && U.app.available ? (S.portable ? 'Скачать' : 'Обновить') : 'Проверить');
  setText($('#zapUpd'), U && U.zapret.available ? 'Обновить' : 'Проверить');
  const show = !!(U && U.app.available);
  const ub = $('#updBar');
  if (!ub.disabled) setHidden(ub, !show);
  if (show && !ub.disabled) setText($('#updText'), `${S.portable ? 'Скачать' : 'Обновить до'} ${U.app.latest}`);
}
async function checkUpdates(quiet) {
  try { U = await invoke('check_updates'); drawUpdates(); if (!quiet && !U.app.available && !U.zapret.available) toast('Всё обновлено'); }
  catch (e) { if (!quiet) toast(String(e)); }
}
async function updateApp(btn) {
  if (!U || !U.app.available) return checkUpdates(false);
  const label = btn.id === 'updBar' ? $('#updText') : btn;
  btn.disabled = true; label.textContent = S.portable ? 'Открываю…' : 'Скачиваю…';
  try { await invoke('update_app'); toast(S.portable ? 'Скачайте новую версию со страницы релиза' : 'Запускаю установщик'); }
  catch (e) { toast(String(e)); }
  btn.disabled = false; drawUpdates();
}
$('#appUpd').onclick = e => updateApp(e.currentTarget);
$('#updBar').onclick = e => updateApp(e.currentTarget);
$('#zapUpd').onclick = async e => {
  const b = e.currentTarget;
  if (picking) { toast(PICK_WAIT); return; }
  if (!U || !U.zapret.available) return checkUpdates(false);
  b.disabled = true; b.textContent = 'Обновляю…';
  try { await call('update_zapret', {}, 'Обходы обновлены'); U.zapret.available = false; U.zapret.current = S.zapret_version; } catch {}
  b.disabled = false; drawUpdates(); invalidate(); checkSoon();
};
listen('updates', e => { U = e.payload; drawUpdates(); });
listen('zapret-updated', e => toast(`Обходы обновлены до версии ${e.payload}`));
// Сообщения от программы: сторож выключил обход, сменилась сеть и так далее.
listen('notice', e => toast(String(e.payload)));

listen('state-changed', () => refresh().then(checkSoon, () => {}));
refresh().then(checkSoon, () => {});
// Если программа долго не отвечает, окно всё равно появляется.
setTimeout(ready, 1500);
