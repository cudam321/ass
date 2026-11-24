// Service worker of the extension test. Every listener is registered at the top, synchronously,
// because Chrome only wakes a worker for listeners that exist when its script first runs.
importScripts('config.js', 'lib.js');

var L = self.assLib;
var T = self.ASS_TEST;
var bootId = 'boot-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 7);
L.bootId = bootId;

var S = {
  wake: null,            // null until decided: true when the suite of this launch is already done
  queue: [],             // events that arrived before the wake decision
  handled: [],           // names of the events this boot handled
  fired: {},             // event check id -> true once recorded
  absentEvents: [],
  helloSender: null,
  helloReceived: false,
  victimTabId: null,
  suiteStarted: false,
  wakeRecorded: false,
  suiteDoneAt: null,
  changes: [],           // storage.onChanged log: {area, keys, t}
  alarmAt: 0,
  keepalive: null,
  testTabId: null,
  windowId: -1,
  createdTabs: [],
  createdWindow: null,
  createdWindowRemoved: false
};

var EVENT_IDS = [
  'runtime.onInstalled',
  'tabs.onCreated', 'tabs.onUpdated', 'tabs.onActivated', 'tabs.onRemoved',
  'windows.onCreated', 'windows.onRemoved', 'windows.onFocusChanged', 'windows.onBoundsChanged',
  'webNavigation.onBeforeNavigate', 'webNavigation.onCommitted',
  'webNavigation.onDOMContentLoaded', 'webNavigation.onCompleted',
  'alarms.onAlarm', 'dnr.onRuleMatchedDebug'
];

function errMsg(e) { return L.errMsg(e); }
function onServer(url) {
  return !!url && ((!!T.server && url.indexOf(T.server) === 0) || (!!T.xserver && url.indexOf(T.xserver) === 0));
}

function note(name, detail, count) {
  if (S.wake === null) { S.queue.push([name, detail, count]); return; }
  handle(name, detail, count);
}

function handle(name, detail, count) {
  if (S.handled.indexOf(name) < 0) S.handled.push(name);
  if (S.wake && !S.wakeRecorded) {
    S.wakeRecorded = true;
    L.record({
      id: 'worker.wakes_after_idle', group: 'worker', result: 'works',
      detail: {
        wokenBy: name, bootId: bootId,
        secondsAfterSuiteDone: S.suiteDoneAt ? Math.round((Date.now() - S.suiteDoneAt) / 1000) : null
      }
    });
  }
  if (count && EVENT_IDS.indexOf(name) >= 0 && !S.fired[name]) {
    S.fired[name] = true;
    var bad = name === 'runtime.onInstalled' && detail && detail.reason !== 'install';
    L.record({
      id: name, group: groupOf(name), result: bad ? 'fails' : 'works',
      detail: bad ? 'reason was ' + detail.reason : detail
    });
  }
}

function groupOf(id) {
  var g = id.split('.')[0];
  return {runtime: 'runtime', tabs: 'tabs', windows: 'windows', webNavigation: 'web_navigation',
          alarms: 'alarms', dnr: 'dnr'}[g];
}

// accept(...args) decides whether the event counts for its check; describe(...args) makes the detail.
function listen(id, ev, accept, describe) {
  if (!ev) { S.absentEvents.push(id); return; }
  try {
    ev.addListener(function () {
      var d, count = true;
      try {
        if (accept) count = !!accept.apply(null, arguments);
        d = describe.apply(null, arguments);
      } catch (e) { d = {error: errMsg(e)}; }
      note(id, d, count);
    });
  } catch (e) {
    S.listenErrors = S.listenErrors || {};
    S.listenErrors[id] = errMsg(e);
  }
}

listen('runtime.onInstalled', L.resolve('chrome.runtime.onInstalled'), null,
  function (d) { return {reason: d.reason}; });
listen('tabs.onCreated', L.resolve('chrome.tabs.onCreated'), null,
  function (t) { return {tabId: t.id, url: t.url || t.pendingUrl}; });
listen('tabs.onUpdated', L.resolve('chrome.tabs.onUpdated'), null,
  function (tabId, change, t) { return {tabId: tabId, change: Object.keys(change), url: t && t.url}; });
listen('tabs.onActivated', L.resolve('chrome.tabs.onActivated'), null,
  function (a) { return {tabId: a.tabId, windowId: a.windowId}; });
listen('tabs.onRemoved', L.resolve('chrome.tabs.onRemoved'), null,
  function (tabId, info) { return {tabId: tabId, windowClosing: info && info.isWindowClosing}; });
listen('windows.onCreated', L.resolve('chrome.windows.onCreated'), null,
  function (w) { return {windowId: w.id}; });
listen('windows.onRemoved', L.resolve('chrome.windows.onRemoved'), null,
  function (id) { return {windowId: id}; });
listen('windows.onFocusChanged', L.resolve('chrome.windows.onFocusChanged'), null,
  function (id) { return {windowId: id}; });
listen('windows.onBoundsChanged', L.resolve('chrome.windows.onBoundsChanged'), null,
  function (w) { return {windowId: w.id}; });
['onBeforeNavigate', 'onCommitted', 'onDOMContentLoaded', 'onCompleted'].forEach(function (name) {
  var id = {
    onBeforeNavigate: 'webNavigation.onBeforeNavigate', onCommitted: 'webNavigation.onCommitted',
    onDOMContentLoaded: 'webNavigation.onDOMContentLoaded', onCompleted: 'webNavigation.onCompleted'
  }[name];
  listen(id, L.resolve('chrome.webNavigation') && chrome.webNavigation[name],
    function (d) { return onServer(d.url); },
    function (d) { return {tabId: d.tabId, frameId: d.frameId, url: d.url}; });
});
listen('alarms.onAlarm', L.resolve('chrome.alarms.onAlarm'),
  function (a) { return a.name === 'ass-check'; },
  function (a) { return {name: a.name}; });
listen('dnr.onRuleMatchedDebug', L.resolve('chrome.declarativeNetRequest.onRuleMatchedDebug'),
  function (i) { return onServer(i.request && i.request.url); },
  function (i) { return {ruleId: i.rule && i.rule.ruleId, rulesetId: i.rule && i.rule.rulesetId, url: i.request && i.request.url}; });

// action.onClicked: registration only. Firing needs a toolbar button.
var clickState = 'absent';
var clickEv = L.resolve('chrome.action.onClicked');
if (clickEv) {
  try {
    clickEv.addListener(function () { note('chrome.action.onClicked', {}, false); });
    clickState = 'ok';
  } catch (e) { clickState = errMsg(e); }
}

// The three registrations below are guarded: an exception at the top of a worker script stops the
// whole script, and every later check with it. A missing API shows up in that API's own checks.
try {
chrome.storage.onChanged.addListener(function (changes, area) {
  S.changes.push({area: area, keys: Object.keys(changes), t: Date.now()});
  note('chrome.storage.onChanged', {area: area}, false);
});
} catch (e) { S.listenErrors = S.listenErrors || {}; S.listenErrors['chrome.storage.onChanged'] = errMsg(e); }

try {
chrome.runtime.onMessage.addListener(function (msg, sender, sendResponse) {
  var type = msg && msg.type;
  note('chrome.runtime.onMessage', {type: type}, false);
  if (type === 'hello') {
    S.helloSender = S.helloSender || sender;
    S.helloReceived = true;
    sendResponse({type: 'hello-ack', tabId: sender.tab && sender.tab.id});
    maybeStart(false);
  } else if (type === 'victim-hello') {
    S.victimTabId = sender.tab ? sender.tab.id : null;
    sendResponse({type: 'victim-ack'});
  } else if (type === 'popup-ping') {
    sendResponse({type: 'popup-pong'});
  }
  return false;
});
} catch (e) { S.listenErrors = S.listenErrors || {}; S.listenErrors['chrome.runtime.onMessage'] = errMsg(e); }

try {
chrome.runtime.onConnect.addListener(function (port) {
  note('chrome.runtime.onConnect', {name: port.name}, false);
  if (port.name !== 'ass-port') return;
  port.onMessage.addListener(function (m) {
    try { port.postMessage({echo: m}); } catch (e) {}
  });
});
} catch (e) { S.listenErrors = S.listenErrors || {}; S.listenErrors['chrome.runtime.onConnect'] = errMsg(e); }

// ---- Boot: decide whether this is the first start or a wake after idle ----

(async function boot() {
  var done = await L.hasRaw('ctl.suite.done');
  var sess = {};
  try { sess = await chrome.storage.session.get(['suiteDone', 'suiteDoneAt', 'suiteStarted']); } catch (e) {}
  S.suiteDoneAt = sess.suiteDoneAt || null;
  // The server answers for the current launch. If it cannot be reached, use the session flag.
  var wake = done === null ? !!sess.suiteDone : done;
  S.wake = wake;
  await L.record({id: 'ctl.worker.boot', group: 'control', result: 'works', detail: {bootId: bootId, wake: wake, listenErrors: S.listenErrors || null}});

  if (!wake) {
    L.record({id: 'worker.starts', group: 'worker', result: 'works', detail: {bootId: bootId}});
    S.absentEvents.forEach(function (id) {
      S.fired[id] = true;
      L.record({id: id, group: groupOf(id), result: 'absent', detail: 'event object is undefined'});
    });
    if (clickState === 'absent') {
      L.record({id: 'action.onClicked', group: 'action', result: 'absent', detail: 'event object is undefined'});
    } else if (clickState === 'ok') {
      L.record({id: 'action.onClicked', group: 'action', result: 'not-run',
        detail: 'listener registered; firing needs a click on a toolbar button, which the app does not have'});
    } else {
      L.record({id: 'action.onClicked', group: 'action', result: 'fails', detail: clickState});
    }
  }
  S.queue.splice(0).forEach(function (q) { handle(q[0], q[1], q[2]); });

  if (!wake) {
    if (sess.suiteStarted) {
      // The worker restarted in the middle of a suite. Do not run it twice.
      S.resumed = true;
      return;
    }
    maybeStart(false);
    setTimeout(function () { maybeStart(true); }, 15000);
  }
})();

function maybeStart(force) {
  if (S.wake !== false || S.suiteStarted || S.resumed) return;
  if (!S.helloReceived && !force) return;
  S.suiteStarted = true;
  runSuite();
}

// ---- The suite ----

var NO_TAB = 'no test tab id: hello message, tabs.query and the tabs.get probe all found none';

async function findTab(path, hintId) {
  if (typeof hintId === 'number') return {id: hintId, foundBy: 'hello'};
  try {
    var r = await chrome.tabs.query({url: 'http://127.0.0.1/' + path + '*'});
    if (r && r.length) return {id: r[0].id, foundBy: 'query'};
  } catch (e) {}
  for (var i = 1; i <= 64; i++) {
    try {
      var t = await chrome.tabs.get(i);
      var u = (t && (t.url || t.pendingUrl)) || '';
      if (T.server && u.indexOf(T.server + '/' + path) === 0) return {id: i, foundBy: 'probe'};
    } catch (e) {}
  }
  return null;
}

function waitChange(area, key, ms) {
  var end = Date.now() + ms;
  return new Promise(function (resolve) {
    (function poll() {
      var hit = S.changes.some(function (c) { return c.area === area && c.keys.indexOf(key) >= 0; });
      if (hit) return resolve(true);
      if (Date.now() >= end) return resolve(false);
      setTimeout(poll, 50);
    })();
  });
}

async function runSteps(steps) {
  for (var i = 0; i < steps.length; i++) {
    var s = steps[i];
    if (s.needTab && S.testTabId === null) {
      if (!L.skipped(s.id)) {
        await L.record({id: s.id, group: s.group, result: 'not-run', detail: NO_TAB});
      }
      continue;
    }
    await L.check(s.id, s.group, s.fn, s.opts);
  }
}

function stepsA() {
  return [
    {id: 'runtime.id', group: 'runtime', fn: function () {
      return chrome.runtime.id === T.extensionId ? L.works(chrome.runtime.id)
        : L.fails('id is ' + chrome.runtime.id + ', expected ' + T.extensionId);
    }},
    {id: 'runtime.getManifest', group: 'runtime', fn: function () {
      var m = chrome.runtime.getManifest();
      return m && m.manifest_version === 3 && m.name === 'Ass extension test'
        ? L.works({manifest_version: m.manifest_version, name: m.name}) : L.fails(L.shape(m));
    }},
    {id: 'storage.local.set_get', group: 'storage', fn: async function () {
      var v = 'v' + Date.now();
      await chrome.storage.local.set({assTest: v, fromWorker: 'set-by-worker'});
      var got = await chrome.storage.local.get('assTest');
      return got.assTest === v ? L.works('round trip ok') : L.fails('got ' + L.shape(got));
    }},
    {id: 'storage.local.onChanged', group: 'storage', fn: async function () {
      await chrome.storage.local.set({assChange: Date.now()});
      return (await waitChange('local', 'assChange', 3000)) ? L.works('fired') : L.fails('not fired within 3 s');
    }},
    {id: 'storage.session.set_get', group: 'storage', fn: async function () {
      var v = 'v' + Date.now();
      await chrome.storage.session.set({assSess: v});
      var got = await chrome.storage.session.get('assSess');
      return got.assSess === v ? L.works('round trip ok') : L.fails('got ' + L.shape(got));
    }},
    {id: 'storage.session.onChanged', group: 'storage', fn: async function () {
      await chrome.storage.session.set({assSessChange: Date.now()});
      return (await waitChange('session', 'assSessChange', 3000)) ? L.works('fired') : L.fails('not fired within 3 s');
    }},
    {id: 'dnr.static_ruleset_enabled', group: 'dnr', fn: async function () {
      var ids = await chrome.declarativeNetRequest.getEnabledRulesets();
      return ids.indexOf('static_rules') >= 0 ? L.works(ids) : L.fails('enabled: ' + JSON.stringify(ids));
    }},
    {id: 'dnr.dynamic.add', group: 'dnr', fn: async function () {
      var d = chrome.declarativeNetRequest;
      await d.updateDynamicRules({removeRuleIds: [100]});
      await d.updateDynamicRules({addRules: [{
        id: 100, priority: 1, action: {type: 'block'},
        condition: {urlFilter: '/__dnr/dynamic-block.txt', resourceTypes: ['xmlhttprequest']}
      }]});
      var rules = await d.getDynamicRules();
      return rules.some(function (r) { return r.id === 100; })
        ? L.works('rule 100 listed') : L.fails('rules: ' + L.shape(rules));
    }},
    {id: 'alarms.create', group: 'alarms', fn: async function () {
      S.alarmAt = Date.now();
      await chrome.alarms.create('ass-check', {delayInMinutes: 0.5});
      var a = await chrome.alarms.get('ass-check');
      return a && a.name === 'ass-check' ? L.works({scheduledTime: a.scheduledTime}) : L.fails('get returned ' + L.shape(a));
    }},
    {id: 'contextMenus.create', group: 'context_menus', fn: function () {
      var create = L.api('chrome.contextMenus.create');
      return new Promise(function (resolve) {
        create({id: 'ass-menu', title: 'Ass test', contexts: ['page']}, function () {
          var e = chrome.runtime.lastError;
          resolve(e ? L.fails(e.message) : L.works('no lastError'));
        });
      });
    }},
    {id: 'permissions.contains', group: 'permissions', fn: async function () {
      var ok = await chrome.permissions.contains({permissions: ['tabs'], origins: ['http://127.0.0.1/*']});
      return ok === true ? L.works(true) : L.fails('resolved ' + ok);
    }},
    {id: 'commands.getAll', group: 'commands', fn: async function () {
      var list = await chrome.commands.getAll();
      var c = list.filter(function (x) { return x.name === 'ass-test-command'; })[0];
      if (!c) return L.fails('command not listed: ' + L.shape(list));
      return c.shortcut ? L.works({shortcut: c.shortcut}) : L.fails('declared, no shortcut registered');
    }},
    {id: 'action.setBadgeText', group: 'action', fn: async function () {
      await chrome.action.setBadgeText({text: 'ok'});
      var t = await chrome.action.getBadgeText({});
      return t === 'ok' ? L.works(t) : L.fails('getBadgeText returned ' + JSON.stringify(t));
    }},
    {id: 'action.setIcon', group: 'action', fn: async function () {
      var canvas = new OffscreenCanvas(16, 16);
      var ctx = canvas.getContext('2d');
      ctx.fillStyle = '#c33';
      ctx.fillRect(0, 0, 16, 16);
      await chrome.action.setIcon({imageData: ctx.getImageData(0, 0, 16, 16)});
      return L.works('resolved');
    }},
    {id: 'action.setPopup', group: 'action', fn: async function () {
      await chrome.action.setPopup({popup: 'popup.html'});
      var p = await chrome.action.getPopup({});
      return /\/popup\.html$/.test(p || '') ? L.works(p) : L.fails('getPopup returned ' + JSON.stringify(p));
    }},
    {id: 'cookies.get', group: 'cookies', fn: async function () {
      var c = await chrome.cookies.get({url: T.server + '/', name: 'ass_server_cookie'});
      return c && c.value === '1' ? L.works({value: c.value}) : L.fails('cookie: ' + L.shape(c));
    }},
    {id: 'cookies.set', group: 'cookies', fn: async function () {
      await chrome.cookies.set({url: T.server + '/', name: 'ass_ext_cookie', value: 'set-by-extension'});
      var c = await chrome.cookies.get({url: T.server + '/', name: 'ass_ext_cookie'});
      return c && c.value === 'set-by-extension' ? L.works({value: c.value}) : L.fails('cookie: ' + L.shape(c));
    }}
  ];
}

function stepsB() {
  var TAB = true;
  function exec(opts) {
    return chrome.scripting.executeScript(Object.assign({target: {tabId: S.testTabId}}, opts));
  }
  return [
    {id: 'msg.tabs.sendMessage', group: 'messaging', needTab: TAB, fn: async function () {
      var r = await chrome.tabs.sendMessage(S.testTabId, {type: 'worker-ping'}, {frameId: 0});
      return r && r.type === 'cs-pong' ? L.works(r) : L.fails('reply: ' + L.shape(r));
    }},
    {id: 'msg.sender.tab', group: 'messaging', needTab: TAB, fn: function () {
      var s = S.helloSender;
      if (!s) return L.fails('no hello message arrived');
      var ok = s.tab && typeof s.tab.id === 'number' && onServer(s.tab.url) &&
               s.tab.url.indexOf('/start.html') >= 0 && s.frameId === 0;
      var d = {tabId: s.tab && s.tab.id, url: s.tab && s.tab.url, frameId: s.frameId};
      return ok ? L.works(d) : L.fails(d);
    }},
    {id: 'scripting.executeScript', group: 'scripting', needTab: TAB, fn: async function () {
      var r = await exec({func: function () { return document.title; }});
      var v = r && r[0] && r[0].result;
      return v === 'ass-extension-start' ? L.works(v) : L.fails('result: ' + L.shape(r));
    }},
    {id: 'scripting.executeScript.main_world', group: 'scripting', needTab: TAB, fn: async function () {
      var r = await exec({world: 'MAIN', func: function () { return window.assPageSecret; }});
      var v = r && r[0] && r[0].result;
      return v === 'page-only' ? L.works(v) : L.fails('result: ' + L.shape(r));
    }},
    {id: 'scripting.insertCSS', group: 'scripting', needTab: TAB, fn: async function () {
      await chrome.scripting.insertCSS({target: {tabId: S.testTabId}, css: '#css-target { width: 123px !important; }'});
      var r = await exec({func: function () { return getComputedStyle(document.getElementById('css-target')).width; }});
      var v = r && r[0] && r[0].result;
      return v === '123px' ? L.works(v) : L.fails('computed width: ' + L.shape(r));
    }},
    {id: 'scripting.registerContentScripts', group: 'scripting', needTab: TAB, fn: async function () {
      var sc = chrome.scripting;
      try { await sc.unregisterContentScripts({ids: ['ass-registered']}); } catch (e) {}
      await sc.registerContentScripts([{
        id: 'ass-registered', js: ['config.js', 'lib.js', 'registered.js'],
        matches: ['http://127.0.0.1/nav*'], runAt: 'document_idle'
      }]);
      var list = await sc.getRegisteredContentScripts({ids: ['ass-registered']});
      return list && list.length === 1 ? L.works('listed') : L.fails('listed: ' + L.shape(list));
    }},
    {id: 'webNavigation.getAllFrames', group: 'web_navigation', needTab: TAB, fn: async function () {
      var f = await chrome.webNavigation.getAllFrames({tabId: S.testTabId});
      var urls = (f || []).map(function (x) { return x.url; });
      return f && f.length === 3 ? L.works(urls) : L.fails('frames: ' + (f ? f.length : f) + ' ' + JSON.stringify(urls).slice(0, 200));
    }},
    {id: 'tabs.query.all', group: 'tabs', needTab: TAB, fn: async function () {
      var r = await chrome.tabs.query({});
      if (!r.length) return L.fails('returned 0 tabs');
      return r.some(function (t) { return t.id === S.testTabId; })
        ? L.works(r.length + ' tabs') : L.fails(r.length + ' tabs, none is the test tab');
    }},
    {id: 'tabs.query.active', group: 'tabs', needTab: TAB, fn: async function () {
      var r = await chrome.tabs.query({active: true});
      return r.length >= 1 ? L.works(r.length + ' active') : L.fails('returned 0 tabs');
    }},
    {id: 'tabs.query.active_current_window', group: 'tabs', needTab: TAB, fn: async function () {
      var r = await chrome.tabs.query({active: true, lastFocusedWindow: true});
      return r.length === 1 ? L.works({tabId: r[0].id}) : L.fails('returned ' + r.length + ' tabs');
    }},
    {id: 'tabs.query.url', group: 'tabs', needTab: TAB, fn: async function () {
      var r = await chrome.tabs.query({url: 'http://127.0.0.1/start.html*'});
      if (!r.length) return L.fails('returned 0 tabs');
      return r.some(function (t) { return t.id === S.testTabId; })
        ? L.works(r.length + ' tabs') : L.fails('returned ' + r.length + ' tabs, none is the test tab');
    }},
    {id: 'tabs.get', group: 'tabs', needTab: TAB, fn: async function () {
      var t = await chrome.tabs.get(S.testTabId);
      if (typeof t.windowId === 'number') S.windowId = t.windowId;
      var d = {windowId: t.windowId, index: t.index, url: t.url};
      return t.url && t.url.indexOf('/start.html') >= 0 ? L.works(d) : L.fails(d);
    }},
    {id: 'tabs.getCurrent.worker', group: 'tabs', needTab: TAB, fn: async function () {
      var t = await chrome.tabs.getCurrent();
      return t === undefined ? L.works('resolved undefined') : L.fails('resolved ' + L.shape(t));
    }},
    {id: 'tabs.update.muted', group: 'tabs', needTab: TAB, fn: async function () {
      try {
        await chrome.tabs.update(S.testTabId, {muted: true});
        var t = await chrome.tabs.get(S.testTabId);
        var ok = t.mutedInfo && t.mutedInfo.muted === true;
        return ok ? L.works(t.mutedInfo) : L.fails('mutedInfo: ' + L.shape(t.mutedInfo));
      } finally {
        try { await chrome.tabs.update(S.testTabId, {muted: false}); } catch (e) {}
      }
    }},
    {id: 'tabs.update.active', group: 'tabs', needTab: TAB, fn: async function () {
      var t = await chrome.tabs.update(S.testTabId, {active: true});
      return t && t.active === true ? L.works('active') : L.fails('resolved ' + L.shape(t));
    }},
    {id: 'tabs.captureVisibleTab', group: 'tabs', needTab: TAB, fn: async function () {
      var args = S.windowId >= 0 ? [S.windowId, {format: 'png'}] : [{format: 'png'}];
      var url = await chrome.tabs.captureVisibleTab.apply(chrome.tabs, args);
      return typeof url === 'string' && url.indexOf('data:image') === 0
        ? L.works({length: url.length}) : L.fails('resolved ' + L.shape(url));
    }},
    {id: 'windows.getAll', group: 'windows', needTab: TAB, fn: async function () {
      var w = await chrome.windows.getAll({populate: true});
      if (!w.length) return L.fails('returned 0 windows');
      var has = w.some(function (x) { return (x.tabs || []).some(function (t) { return t.id === S.testTabId; }); });
      return has ? L.works(w.length + ' windows') : L.fails(w.length + ' windows, none holds the test tab');
    }},
    {id: 'windows.getCurrent', group: 'windows', needTab: TAB, fn: async function () {
      var w = await chrome.windows.getCurrent();
      return w && typeof w.id === 'number' ? L.works({id: w.id}) : L.fails('resolved ' + L.shape(w));
    }},
    {id: 'windows.getLastFocused', group: 'windows', needTab: TAB, fn: async function () {
      var w = await chrome.windows.getLastFocused();
      return w && typeof w.id === 'number' ? L.works({id: w.id}) : L.fails('resolved ' + L.shape(w));
    }},
    {id: 'windows.get', group: 'windows', needTab: TAB, fn: async function () {
      var useTab = S.windowId >= 0;
      var id = useTab ? S.windowId : chrome.windows.WINDOW_ID_CURRENT;
      var w = await chrome.windows.get(id);
      var d = {idFrom: useTab ? 'the test tab window id' : 'WINDOW_ID_CURRENT', id: w && w.id};
      return w && typeof w.id === 'number' ? L.works(d) : L.fails('resolved ' + L.shape(w));
    }},
    {id: 'dnr.getMatchedRules', group: 'dnr', needTab: TAB, opts: {timeoutMs: 40000}, fn: async function () {
      var done = await L.waitFor('ctl.page.dnr_done', 30000);
      var r = await chrome.declarativeNetRequest.getMatchedRules({tabId: S.testTabId});
      var list = (r && r.rulesMatchedInfo) || [];
      var s1 = list.some(function (x) { return x.rule.ruleId === 1 && x.rule.rulesetId === 'static_rules'; });
      var d100 = list.some(function (x) { return x.rule.ruleId === 100; });
      var d = {static1: s1, dynamic100: d100, matches: list.length, pageDone: done};
      return s1 && d100 ? L.works(d) : L.fails(d);
    }}
  ];
}

function stepsC() {
  var TAB = true, RISKY = {risky: true};
  var noVictim = 'no victim tab id';
  return [
    {id: 'tabs.create', group: 'tabs', needTab: TAB, opts: {risky: true, timeoutMs: 15000}, fn: async function () {
      var t = await chrome.tabs.create({url: T.server + '/created-tab.html', active: false});
      if (!t || typeof t.id !== 'number') return L.fails('resolved ' + L.shape(t));
      S.createdTabs.push(t.id);
      var ok = await L.waitFor('ctl.created_tab.loaded', 10000);
      return ok ? L.works({tabId: t.id}) : L.fails('tab ' + t.id + ' created but created-tab.html did not load within 10 s');
    }},
    {id: 'tabs.duplicate', group: 'tabs', needTab: TAB, opts: RISKY, fn: async function () {
      if (S.victimTabId === null) return L.notRun(noVictim);
      var t = await chrome.tabs.duplicate(S.victimTabId);
      if (t && typeof t.id === 'number') S.createdTabs.push(t.id);
      return t && typeof t.id === 'number' ? L.works({tabId: t.id}) : L.fails('resolved ' + L.shape(t));
    }},
    {id: 'tabs.move', group: 'tabs', needTab: TAB, opts: RISKY, fn: async function () {
      if (S.victimTabId === null) return L.notRun(noVictim);
      var r = await chrome.tabs.move(S.victimTabId, {index: 0});
      var t = Array.isArray(r) ? r[0] : r;
      return t && t.index === 0 ? L.works({index: t.index}) : L.fails('resolved ' + L.shape(r));
    }},
    {id: 'tabs.group', group: 'tabs', needTab: TAB, opts: RISKY, fn: async function () {
      if (S.victimTabId === null) return L.notRun(noVictim);
      var group = L.api('chrome.tabs.group');
      var g = await group({tabIds: [S.victimTabId]});
      return typeof g === 'number' ? L.works({groupId: g}) : L.fails('resolved ' + L.shape(g));
    }},
    {id: 'windows.create', group: 'windows', needTab: TAB, opts: {risky: true, timeoutMs: 15000}, fn: async function () {
      var w = await chrome.windows.create({url: T.server + '/created-window.html', focused: false});
      if (!w || typeof w.id !== 'number') return L.fails('resolved ' + L.shape(w));
      S.createdWindow = w.id;
      var ok = await L.waitFor('ctl.created_window.loaded', 10000);
      return ok ? L.works({windowId: w.id}) : L.fails('window ' + w.id + ' created but created-window.html did not load within 10 s');
    }},
    {id: 'windows.update', group: 'windows', needTab: TAB, opts: RISKY, fn: async function () {
      var id, change;
      if (S.createdWindow !== null) { id = S.createdWindow; change = {width: 640, height: 480}; }
      else if (S.windowId >= 0) { id = S.windowId; change = {state: 'normal'}; }
      else return L.notRun('no window id: windows.create made none and tabs.get gave no windowId');
      var w = await chrome.windows.update(id, change);
      return w && typeof w.id === 'number'
        ? L.works({windowId: w.id, on: S.createdWindow !== null ? 'created window' : 'test window'})
        : L.fails('resolved ' + L.shape(w));
    }},
    {id: 'windows.remove', group: 'windows', needTab: TAB, opts: RISKY, fn: async function () {
      if (S.createdWindow === null) return L.notRun('windows.create made no window');
      await chrome.windows.remove(S.createdWindow);
      S.createdWindowRemoved = true;
      return L.works({windowId: S.createdWindow});
    }},
    {id: 'tabs.remove', group: 'tabs', needTab: TAB, opts: RISKY, fn: async function () {
      var v = S.victimTabId;
      if (v === null) return L.notRun(noVictim);
      if (v === S.testTabId) return L.notRun('victim id equals the test tab id');
      await chrome.tabs.remove(v);
      for (var i = 0; i < 10; i++) {
        try { await chrome.tabs.get(v); } catch (e) { return L.works('tabs.get rejects after remove'); }
        await L.sleep(200);
      }
      return L.fails('tab ' + v + ' still exists 2 s after remove');
    }}
  ];
}

function stepsC2() {
  var TAB = true;
  return [
    {id: 'action.openPopup', group: 'action', needTab: TAB, opts: {risky: true}, fn: async function () {
      await L.api('chrome.action.openPopup')();
      return L.works('resolved');
    }},
    {id: 'runtime.openOptionsPage', group: 'runtime', needTab: TAB, opts: {risky: true}, fn: async function () {
      await chrome.runtime.openOptionsPage();
      return L.works('resolved');
    }}
  ];
}

function stepsD() {
  var TAB = true;
  return [
    // ctl.page.ready already arrived from the first load, so the server's count of it must grow.
    // Judged on the server alone, so a failure of chrome.scripting cannot fail this check.
    {id: 'tabs.reload', group: 'tabs', needTab: TAB, opts: {timeoutMs: 15000}, fn: async function () {
      var before = await L.count('ctl.page.ready');
      if (before === null) return L.fails('the page server cannot be reached, so a reload cannot be seen');
      var t0 = Date.now();
      await chrome.tabs.reload(S.testTabId);
      while (Date.now() < t0 + 10000) {
        await L.sleep(500);
        if ((await L.count('ctl.page.ready')) > before) return L.works({readyAfterMs: Date.now() - t0});
      }
      return L.fails('the page did not post ctl.page.ready again within 10 s');
    }},
    {id: 'tabs.update.url', group: 'tabs', needTab: TAB, opts: {timeoutMs: 15000}, fn: async function () {
      await chrome.tabs.update(S.testTabId, {url: T.server + '/nav.html'});
      var ok = await L.waitFor('ctl.nav.loaded', 10000);
      return ok ? L.works('nav.html loaded') : L.fails('nav.html did not load within 10 s');
    }}
  ];
}

async function cleanup() {
  var removed = [], errors = [];
  for (var i = 0; i < S.createdTabs.length; i++) {
    var id = S.createdTabs[i];
    if (id === S.testTabId) continue;
    try { await chrome.tabs.remove(id); removed.push(id); } catch (e) { errors.push('tab ' + id + ': ' + errMsg(e)); }
  }
  var leftWindow = null;
  if (S.createdWindow !== null && !S.createdWindowRemoved && S.createdWindow !== S.windowId) {
    try { await chrome.windows.remove(S.createdWindow); leftWindow = 'removed'; }
    catch (e) { leftWindow = 'error: ' + errMsg(e); }
  }
  await L.record({id: 'ctl.cleanup', group: 'control', result: 'works', detail: {
    createdTabs: S.createdTabs, removedTabs: removed, createdWindow: S.createdWindow,
    windowRemovedByCheck: S.createdWindowRemoved, windowCleanup: leftWindow, errors: errors
  }});
}

async function runSuite() {
  try { await chrome.storage.session.set({suiteStarted: true}); } catch (e) {}
  // Keep the worker alive while the suite runs.
  S.keepalive = setInterval(L.ping, 10000);
  var suiteError = null;
  try {
    await runSteps(stepsA());

    var found = null;
    if (S.helloSender && S.helloSender.tab && typeof S.helloSender.tab.id === 'number') {
      found = {id: S.helloSender.tab.id, foundBy: 'hello'};
    } else {
      found = await findTab('start.html', null);
    }
    S.testTabId = found ? found.id : null;
    await L.record({id: 'ctl.suite.begin', group: 'control', result: 'works',
      detail: {testTabId: S.testTabId, foundBy: found ? found.foundBy : 'none'}});

    await runSteps(stepsB());

    if (S.testTabId !== null && T.risky !== false) {
      await L.waitFor('ctl.victim.loaded', 20000);
      var v = await findTab('victim.html', S.victimTabId);
      S.victimTabId = v ? v.id : null;
    }
    await runSteps(stepsC());
    if (S.testTabId !== null) await cleanup();
    await runSteps(stepsC2());
    await runSteps(stepsD());
  } catch (e) {
    suiteError = errMsg(e);
  }

  // Let the alarm and late events arrive.
  var wait = (S.alarmAt || Date.now()) + 35000 - Date.now();
  if (wait > 0) await L.sleep(wait);
  await L.sleep(3000);

  await Promise.all(EVENT_IDS.filter(function (id) { return !S.fired[id]; }).map(function (id) {
    return L.record({id: id, group: groupOf(id), result: 'fails', detail: 'not fired by the end of the suite', final: false});
  }));
  if (S.handled.length) {
    await L.record({id: 'worker.receives_events', group: 'worker', result: 'works', detail: S.handled});
  } else {
    await L.record({id: 'worker.receives_events', group: 'worker', result: 'fails', detail: 'no event handled'});
  }

  var doneAt = Date.now();
  S.suiteDoneAt = doneAt;
  try { await chrome.storage.session.set({suiteDone: true, suiteDoneAt: doneAt}); } catch (e) {}
  try { await chrome.alarms.create('ass-wake', {delayInMinutes: 50 / 60}); } catch (e) {}
  await L.record({id: 'ctl.suite.done', group: 'control', result: 'works',
    detail: {suiteDoneAt: doneAt, error: suiteError}});
  // From here on the worker does nothing that keeps it alive.
  clearInterval(S.keepalive);
  S.keepalive = null;
}
