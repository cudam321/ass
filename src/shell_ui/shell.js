// The shell page: back, forward and reload, the address field, the tabs.
//
// Page to Rust: POST a JSON object to ass://shell/api/<command>.
// Rust to page: three events that carry no data. 'ass-state' means the state
// changed and the page fetches it; 'ass-focus-address' (Cmd+L) and
// 'ass-new-tab' (Cmd+T) come from the menu.
//
// The page shows Rust's state and keeps none of its own, apart from what the
// user is typing and the suggestions made from it. Titles and addresses come
// from web pages: they reach the DOM
// only as textContent or an aria-label, never as markup. The
// Content-Security-Policy (Trusted Types) rejects innerHTML and similar sinks.
'use strict';

const API = 'ass://shell/api/';

const $ = (id) => document.getElementById(id);
const omni = $('omni');
const input = $('address');
const errorLine = $('address-error');
const list = $('tabs');
const suggestionList = $('suggestions');
const newTabButton = $('new-tab');
const reload = $('reload');

// ---- The mark ---------------------------------------------------------------

// The construction from design/shell-prototype.html: a bowl (superellipse,
// exponent 2.5) on two lobes (2.4), 1.18 times as wide as tall, all three
// meeting at the widest point. It is built lobes up, then turned over,
// so the cleft is at the bottom. Small marks are
// filled in, and the cleft is cut deeper as the mark gets smaller, so that it
// is never less than 3.5 pixels deep (30% of the height at most).
function markPath(width) {
  const H = 2 / 1.18, h = 0.4 * H, b = H - h, pt = 2.4, pb = 2.5, N = 64;
  const depth = Math.min(0.3, Math.max(0.15, Math.round((3.5 / (width / 2) / H) * 100) / 100));
  const ratio = (1 - (1 - depth / 0.4) ** pt) ** (1 / pt), c = ratio / (1 + ratio), a = 1 - c;
  const y0 = h - H / 2;
  const pw = (v, p) => Math.sign(v) * Math.abs(v) ** (2 / p);
  const points = [];
  for (let i = 0; i < N; i++) {
    const u = (i / N) * Math.PI;
    points.push([pw(Math.cos(u), pb), y0 + b * pw(Math.sin(u), pb)]);
  }
  const end = Math.acos(-((c / a) ** (pt / 2)));
  const lobe = [];
  for (let i = 0; i <= N; i++) {
    const u = (i / N) * end;
    lobe.push([c + a * pw(Math.cos(u), pt), y0 - h * pw(Math.sin(u), pt)]);
  }
  for (const [x, y] of lobe) points.push([-x, y]);
  for (let i = N - 1; i > 0; i--) points.push(lobe[i]);
  return 'M' + points.map(([x, y]) => `${x.toFixed(4)} ${(-y).toFixed(4)}`).join('L') + 'Z';
}
for (const width of [18, 16]) {
  document.querySelector(`#mark-${width} path`).setAttribute('d', markPath(width));
}

// ---- The channel to Rust ----------------------------------------------------

const NOT_CONNECTED = 'Not connected';

// A command that failed on the channel: Rust refused it (the message is
// Rust's own text), or no answer came at all.
class ChannelError extends Error {}

// Sends one command. Resolves with the reply, rejects with a ChannelError. A
// command with no answer in 5 seconds counts as a lost channel, so a request
// that hangs never holds the sidebar.
async function call(command, args = {}) {
  let response, reply;
  try {
    response = await fetch(API + command, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(args),
      signal: AbortSignal.timeout(5000),
    });
    reply = await response.json();
  } catch {
    setConnected(false);
    throw new ChannelError(NOT_CONNECTED);
  }
  if (!response.ok) {
    throw new ChannelError(String(reply?.error ?? response.status));
  }
  setConnected(true);
  return reply;
}

// A freshly loaded page can be refused at first and is told when to try
// again (src/shell.rs), so a failure in its first second says nothing unless
// it lasts.
const GRACE_MS = 1000;
let connected = false;
function setConnected(value) {
  connected = value;
  const wait = GRACE_MS - performance.now();
  if (!value && !known && wait > 0) {
    setTimeout(() => connected || setText($('status'), NOT_CONNECTED), wait);
    return;
  }
  setText($('status'), value ? '' : NOT_CONNECTED);
}

// Fetches the state and shows it. At most one request is in flight: a notice
// that arrives meanwhile is answered by one more request once it returns, so
// the page always ends on the latest state and requests never pile up.
let fetching = false;
let stale = false;
async function refresh() {
  stale = true;
  if (fetching) return;
  fetching = true;
  try {
    while (stale) {
      stale = false;
      render(await call('state'));
    }
  } finally {
    fetching = false;
  }
}

// Checks the channel, then shows the state. The browser can refuse the very
// first request of a freshly loaded page; it then sends 'ass-state' once it
// answers, and the page connects again. The same holds after any failure.
const connect = () => call('ping').then(refresh);
// Channel failures are already shown where they matter. Anything else is a
// bug, and is left to reach the console.
const ignore = (error) => {
  if (!(error instanceof ChannelError)) throw error;
};
addEventListener('ass-state', () => (connected ? refresh() : connect()).catch(ignore));

// Commands from the buttons and the tab list. A refusal there is a race with
// the page (a tab that closed a moment ago) and changes nothing; a lost
// channel shows in the status line.
const send = (command, args) => call(command, args).catch(ignore);

// ---- State ------------------------------------------------------------------

let state = { tabs: [], activeId: null, address: '', canGoBack: false, canGoForward: false };
// A state has arrived since the page loaded.
let known = false;

// Writes an attribute or a class only when it changes (null removes it), so an
// update that changes nothing writes nothing.
function setAttr(element, name, value) {
  if (element.getAttribute(name) === value) return;
  if (value === null) element.removeAttribute(name);
  else element.setAttribute(name, value);
}
function setClass(element, name, on) {
  if (element.classList.contains(name) !== on) element.classList.toggle(name, on);
}

// The user is in the field right now: it has the focus and so does the view.
const typing = () => document.activeElement === input && document.hasFocus();

// The address without a user name or password, which a page can use to put a
// trusted name in front of the real host ('https://bank.example:x@evil.example/').
// Rust strips them too. Any other address is kept exactly as Rust sent it.
function withoutCredentials(address) {
  try {
    const url = new URL(address);
    if (!url.username && !url.password) return address;
    url.username = '';
    url.password = '';
    return url.href;
  } catch {
    return address;
  }
}

function render(next) {
  const before = state;
  const first = !known;
  state = {
    tabs: Array.isArray(next.tabs) ? next.tabs : [],
    activeId: next.activeId ?? null,
    address: withoutCredentials(String(next.address ?? '')),
    canGoBack: next.canGoBack === true,
    canGoForward: next.canGoForward === true,
  };
  known = true;
  const active = state.tabs.find((tab) => tab.id === state.activeId);

  const loading = active?.loading === true;
  setAttr($('back'), 'disabled', state.canGoBack ? null : '');
  setAttr($('forward'), 'disabled', state.canGoForward ? null : '');
  setAttr(reload, 'disabled', active ? null : '');
  setAttr(reload, 'aria-label', loading ? 'Stop' : 'Reload');
  setClass(reload, 'busy', loading);
  setClass(omni, 'loading', loading);

  renderTabs(before, first);
  // The user is not in the field: if they typed nothing, or a different tab
  // came to the front, the field goes back to showing the real address. A
  // field left open on an old address would show the wrong site.
  if (editing && !typing() && (!edited || before.activeId !== state.activeId)) endEdit();
  renderAddress();
  if (editing) renderSuggestions();
}

// ---- Tabs -------------------------------------------------------------------

// One row per tab id, kept across updates: a state change touches only the
// rows and fields that changed.
const rows = new Map();

function renderTabs(before, first) {
  const ids = new Set(state.tabs.map((tab) => tab.id));
  const focused = document.activeElement?.closest?.('.tab');
  let refocus = -1;
  for (const [id, row] of rows) {
    if (ids.has(id)) continue;
    if (row.li === focused) refocus = before.tabs.findIndex((tab) => tab.id === id);
    row.li.remove();
    rows.delete(id);
  }

  let next = list.firstElementChild;
  for (const tab of state.tabs) {
    let row = rows.get(tab.id);
    if (!row) {
      row = makeRow(tab.id);
      rows.set(tab.id, row);
    }
    updateRow(row, tab);
    if (row.li === next) next = next.nextElementSibling;
    else list.insertBefore(row.li, next);
  }

  if (refocus >= 0) {
    const neighbour = list.children[Math.min(refocus, list.children.length - 1)];
    (neighbour ? neighbour.querySelector('.tab-main') : newTabButton).focus();
  }
  if (state.activeId !== before.activeId) rows.get(state.activeId)?.li.scrollIntoView({ block: 'nearest' });

  const empty = state.tabs.length === 0;
  $('empty').hidden = !empty;
  // With no tab left, the field is where the next one starts.
  if (empty && (before.tabs.length > 0 || first) && !editing) startEdit(false);
}

function makeRow(id) {
  const li = $('tab-row').content.firstElementChild.cloneNode(true);
  li.dataset.id = String(id);
  return {
    li,
    main: li.querySelector('.tab-main'),
    title: li.querySelector('.title'),
    busy: li.querySelector('.busy'),
    close: li.querySelector('.tab-close'),
    text: null,
    loading: null,
    active: null,
  };
}

function updateRow(row, tab) {
  const title = String(tab.title ?? '');
  if (row.text !== title) {
    row.text = title;
    row.title.textContent = title;
    row.close.setAttribute('aria-label', `Close ${title}`);
  }
  const loading = tab.loading === true;
  if (row.loading !== loading) {
    row.loading = loading;
    row.li.classList.toggle('loading', loading);
    row.busy.textContent = loading ? ', loading' : '';
  }
  const active = tab.id === state.activeId;
  if (row.active !== active) {
    row.active = active;
    row.li.classList.toggle('active', active);
    setAttr(row.main, 'aria-current', active ? 'page' : null);
  }
}

const rowId = (target) => {
  const li = target.closest?.('.tab');
  return li && list.contains(li) ? Number(li.dataset.id) : null;
};

list.addEventListener('click', (event) => {
  const id = rowId(event.target);
  if (id === null) return;
  if (event.target.closest('.tab-close')) send('close', { id });
  else if (event.target.closest('.tab-main')) send('activate', { id });
});
// Middle click closes. Its mousedown would otherwise start scrolling.
list.addEventListener('mousedown', (event) => {
  if (event.button === 1) event.preventDefault();
});
list.addEventListener('auxclick', (event) => {
  const id = rowId(event.target);
  if (event.button === 1 && id !== null) send('close', { id });
});
// Up and down move between tabs; Delete closes the focused one.
list.addEventListener('keydown', (event) => {
  const li = event.target.closest('.tab');
  if (!li || !event.target.matches('.tab-main')) return;
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault();
    const to = event.key === 'ArrowDown' ? li.nextElementSibling : li.previousElementSibling;
    to?.querySelector('.tab-main').focus();
  } else if (event.key === 'Delete') {
    event.preventDefault();
    send('close', { id: Number(li.dataset.id) });
  }
});

$('back').addEventListener('click', () => send('back'));
$('forward').addEventListener('click', () => send('forward'));
reload.addEventListener('click', () => send(reload.classList.contains('busy') ? 'stop' : 'reload'));

// ---- The address field ------------------------------------------------------

// While the user edits the field, nothing from Rust touches its value.
let editing = false;
// The user asked for a new tab (Cmd+T, the New tab row).
let newTab = false;
// The tab in front when editing began. If another comes to the front meanwhile
// (a page opened a popup, or closed itself), Enter opens a new tab: typed text
// never goes into a tab the user did not see when they began.
let editTab = null;
const opensTab = () => newTab || !state.tabs.length || state.activeId !== editTab;
// The text was changed since editing began. Until then there is nothing to suggest.
let edited = false;
// A command from the field is waiting for its answer.
let sending = false;

// At rest the field shows the host in ink and the rest dimmed. The URL parser
// gives the host in its ASCII form, which is what keeps a look-alike host
// visible as such, and the rest percent-encoded, so that no direction mark in
// it can reorder what is shown. Only an http or https host is ever in ink.
function renderAddress() {
  if (!editing && input.value !== state.address) input.value = state.address;
  let scheme = '', host = '', rest = state.address;
  try {
    const url = new URL(state.address);
    if ((url.protocol === 'https:' || url.protocol === 'http:') && url.host) {
      scheme = url.protocol === 'http:' ? 'http://' : '';
      host = url.host;
      rest = url.pathname === '/' && !url.search && !url.hash ? '' : url.pathname + url.search + url.hash;
    } else {
      // about:blank in a popup, a blob: address: all of it dimmed.
      rest = url.href;
    }
  } catch {
    // Not a URL (an empty field): shown as it is.
  }
  setText(omni.querySelector('.scheme'), scheme);
  setText(omni.querySelector('.host-text'), host);
  setText(omni.querySelector('.rest'), rest);
}

function setText(element, text) {
  if (element.textContent !== text) element.textContent = text;
}

function startEdit(asNewTab) {
  editing = true;
  newTab = asNewTab;
  editTab = state.activeId;
  matched = [];
  omni.classList.add('editing');
  newTabButton.classList.toggle('on', asNewTab);
  input.setAttribute('aria-label', asNewTab ? 'New tab: address or search' : 'Address or search');
  input.value = asNewTab ? '' : state.address;
  showError('');
  highlighted = 'go';
  edited = false;
  if (document.activeElement !== input) input.focus();
  selectAll();
  renderSuggestions();
}

// Selects the whole text and shows its beginning: Chrome would scroll a long
// address to its end.
function selectAll() {
  input.setSelectionRange(0, input.value.length, 'backward');
  input.scrollLeft = 0;
}

function endEdit() {
  editing = false;
  newTab = false;
  omni.classList.remove('editing');
  newTabButton.classList.remove('on');
  input.setAttribute('aria-label', 'Address or search');
  showError('');
  closeSuggestions();
  input.value = state.address;
  if (document.activeElement === input) input.blur();
}

function showError(text) {
  // Rust's refusals are short phrases; shown as a sentence.
  const sentence = text && text[0].toUpperCase() + text.slice(1);
  setText(errorLine, sentence);
  errorLine.hidden = !text;
  setClass(omni, 'open', !!text || shown.length > 0);
  setAttr(input, 'aria-invalid', text ? 'true' : null);
  setAttr(input, 'aria-describedby', text ? 'address-error' : null);
}

// A click anywhere on the field at rest starts editing with the whole address
// selected; a click while editing places the caret as usual.
$('omni-row').addEventListener('mousedown', (event) => {
  if (event.button !== 0) return;
  if (!editing) {
    event.preventDefault();
    startEdit(false);
  } else if (event.target !== input) {
    event.preventDefault();
    input.focus();
  }
});
input.addEventListener('focus', () => {
  if (!editing) startEdit(false);
  else renderSuggestions();
});
input.addEventListener('blur', () => {
  if (!editing) return;
  // The window or the view lost focus, not the field: keep what was typed.
  if (document.activeElement === input) {
    closeSuggestions();
    return;
  }
  endEdit();
});
input.addEventListener('input', () => {
  edited = true;
  showError('');
  highlighted = 'go';
  match();
  renderSuggestions();
});
input.addEventListener('keydown', (event) => {
  if (event.isComposing) return;
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault();
    move(event.key === 'ArrowDown' ? 1 : -1);
  } else if (event.key === 'Enter') {
    event.preventDefault();
    if (!event.repeat) choose(highlighted);
  } else if (event.key === 'Escape') {
    event.preventDefault();
    endEdit();
  }
});

addEventListener('ass-focus-address', () => {
  if (editing && !newTab) {
    input.focus();
    selectAll();
  } else startEdit(false);
});
addEventListener('ass-new-tab', () => {
  if (editing && newTab) {
    input.focus();
    selectAll();
  } else startEdit(true);
});
newTabButton.addEventListener('click', () => startEdit(true));

// ---- Suggestions ------------------------------------------------------------

// The key of the highlighted row: 'go', or 'tab-<id>'.
let highlighted = 'go';
const suggestionRows = new Map();

// What Rust will most likely do with the text. Only for the label: Rust
// decides, and its answer says which it did.
function guess(text) {
  if (/\s/.test(text)) return 'search';
  if (/^[a-z][a-z0-9+.-]*:/i.test(text) && !/^[^:/?#]+:\d+([/?#]|$)/.test(text)) return 'address';
  const host = text.split(/[/?#]/)[0].replace(/:\d+$/, '');
  return host.includes('.') || /^localhost$/i.test(host) || host.startsWith('[') ? 'address' : 'search';
}

// Open tabs whose title holds the text. They are found when the user types and
// kept as they were until the next keystroke, titles included: a page that
// renames or closes its tab cannot move a row under the pointer or the
// highlight, nor change what a row says between reading it and choosing it.
let matched = [];
function match() {
  const needle = input.value.trim().toLowerCase();
  matched = !needle ? [] : state.tabs
    .map((tab) => [tab.id, String(tab.title ?? '')])
    .filter(([, title]) => title.toLowerCase().includes(needle))
    .slice(0, 5)
    .map(([id, title]) => ({ key: `tab-${id}`, kind: 'tab', id, verb: 'Switch to', text: title, hint: '' }));
}
const isOpen = (id) => state.tabs.some((tab) => tab.id === id);

// The tab rows, then the one row that says what Enter does.
function suggestions() {
  const text = input.value.trim();
  if (!text || !edited) return [];
  const kind = guess(text);
  return [
    ...matched,
    { key: 'go', kind, verb: kind === 'search' ? 'Search' : 'Go to', text, hint: opensTab() ? 'New tab' : '' },
  ];
}

let shown = [];
function renderSuggestions() {
  shown = editing && typing() ? suggestions() : [];
  if (!shown.some((item) => item.key === highlighted)) highlighted = 'go';
  const keys = new Set(shown.map((item) => item.key));
  for (const [key, li] of suggestionRows) {
    if (!keys.has(key)) {
      li.remove();
      suggestionRows.delete(key);
    }
  }
  let next = suggestionList.firstElementChild;
  for (const item of shown) {
    let li = suggestionRows.get(item.key);
    if (!li) {
      li = $('suggestion-row').content.firstElementChild.cloneNode(true);
      li.id = `suggestion-${item.key}`;
      li.dataset.key = item.key;
      suggestionRows.set(item.key, li);
    }
    setAttr(li, 'data-kind', item.kind);
    setText(li.querySelector('.verb'), item.verb);
    setText(li.querySelector('.text'), item.text);
    setText(li.querySelector('.hint'), item.hint);
    setAttr(li, 'aria-selected', String(item.key === highlighted));
    setAttr(li, 'aria-disabled', item.kind === 'tab' && !isOpen(item.id) ? 'true' : null);
    if (li === next) next = next.nextElementSibling;
    else suggestionList.insertBefore(li, next);
  }
  suggestionList.hidden = shown.length === 0;
  setClass(omni, 'open', shown.length > 0 || !errorLine.hidden);
  setAttr(input, 'aria-expanded', String(shown.length > 0));
  setAttr(input, 'aria-activedescendant', shown.length ? `suggestion-${highlighted}` : null);
}

function closeSuggestions() {
  shown = [];
  for (const li of suggestionRows.values()) li.remove();
  suggestionRows.clear();
  suggestionList.hidden = true;
  setClass(omni, 'open', !errorLine.hidden);
  input.setAttribute('aria-expanded', 'false');
  input.removeAttribute('aria-activedescendant');
}

function move(step) {
  if (!shown.length) return;
  const at = shown.findIndex((item) => item.key === highlighted);
  highlighted = shown[(at + step + shown.length) % shown.length].key;
  renderSuggestions();
  suggestionRows.get(highlighted)?.scrollIntoView({ block: 'nearest' });
}

// Runs the highlighted row: switch to a tab, or hand the text to Rust. A
// refusal is shown under the field and the typed text stays.
async function choose(key) {
  const item = shown.find((row) => row.key === key);
  const text = input.value.trim();
  // A tab row whose tab has closed since does nothing.
  if (sending || (key !== 'go' && !item) || (item?.kind === 'tab' && !isOpen(item.id)) || (item?.kind !== 'tab' && !text)) return;
  let command, args;
  if (item?.kind === 'tab') [command, args] = ['activate', { id: item.id }];
  else [command, args] = [opensTab() ? 'open' : 'navigate', { input: text }];
  sending = true;
  try {
    await call(command, args);
    endEdit();
  } catch (error) {
    ignore(error);
    showError(error.message);
  } finally {
    sending = false;
  }
}

// Clicks on a suggestion keep the focus in the field. The pointer only shows
// hover (CSS): it never moves the highlight, so it never changes what Enter runs.
suggestionList.addEventListener('mousedown', (event) => event.preventDefault());
suggestionList.addEventListener('click', (event) => {
  const li = event.target.closest('.suggestion');
  if (li) choose(li.dataset.key);
});

// ---- Start --------------------------------------------------------------------

connect().catch(ignore);
