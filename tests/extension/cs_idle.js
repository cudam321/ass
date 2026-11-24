// Content script at document_idle. Records only on start.html (main frame) and frame.html (frames).
// Each check is in its own try/catch so one failure cannot stop the next.
(function () {
  var L = self.assLib;
  var top_ = window === top;
  var path = location.pathname;
  var kind = top_ ? 'main_frame'
    : location.hostname === '127.0.0.1' ? 'iframe_same_origin'
    : location.hostname === 'localhost' ? 'iframe_cross_origin' : null;
  var ids = {
    main_frame: 'cs.document_idle.main_frame',
    iframe_same_origin: 'cs.document_idle.iframe_same_origin',
    iframe_cross_origin: 'cs.document_idle.iframe_cross_origin'
  };

  function rec(id, group, r) { L.record({id: id, group: group, result: r.result, detail: r.detail}); }
  function recFail(id, group, e) { L.record({id: id, group: group, result: 'fails', detail: L.errMsg(e)}); }

  // Resolves true when the attribute equals expected within ms, else the last value seen.
  function waitAttr(el, name, expected, ms) {
    return new Promise(function (resolve) {
      var end = Date.now() + ms;
      (function poll() {
        var v = el.getAttribute(name);
        if (v === expected) return resolve(true);
        if (Date.now() >= end) return resolve(v);
        setTimeout(poll, 50);
      })();
    });
  }

  function fill(user) {
    var u = document.getElementById('login-user');
    var p = document.getElementById('login-pass');
    if (!u || !p) throw new Error('#login-user or #login-pass not found');
    u.value = user;
    p.value = user === 'ass-user' ? 'ass-pass-123' : 'ass-frame-pass-123';
    ['input', 'change'].forEach(function (type) {
      u.dispatchEvent(new Event(type, {bubbles: true}));
      p.dispatchEvent(new Event(type, {bubbles: true}));
    });
    return u.form;
  }

  function portCheck() {
    return new Promise(function (resolve) {
      var port = chrome.runtime.connect({name: 'ass-port'});
      var got = 0, finished = false;
      function finish(r) {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        try { port.disconnect(); } catch (e) {}
        resolve(r);
      }
      var timer = setTimeout(function () { finish(L.fails('got ' + got + ' of 3 echoes within 5 s')); }, 5000);
      port.onMessage.addListener(function (m) {
        if (m && m.echo) got++;
        if (got === 3) finish(L.works('3 echoes'));
      });
      port.onDisconnect.addListener(function () { finish(L.fails('port closed after ' + got + ' echoes')); });
      for (var i = 0; i < 3; i++) port.postMessage({n: i});
    });
  }

  async function mainFrame() {
    try {
      var ok = document.documentElement.dataset.assPage === 'start' && typeof window.assPageSecret === 'undefined';
      rec('cs.isolated_world', 'content_scripts', ok ? L.works('sees the DOM, not the page global')
        : L.fails({assPage: document.documentElement.dataset.assPage, assPageSecret: typeof window.assPageSecret}));
    } catch (e) { recFail('cs.isolated_world', 'content_scripts', e); }

    try {
      var v = 'cs' + Date.now();
      await chrome.storage.local.set({assCs: v});
      var got = await chrome.storage.local.get('assCs');
      rec('storage.local.content_script', 'storage', got.assCs === v ? L.works('round trip ok') : L.fails(L.shape(got)));
    } catch (e) { recFail('storage.local.content_script', 'storage', e); }

    try {
      var reply = await L.withTimeout(chrome.runtime.sendMessage({type: 'hello'}), 5000);
      rec('msg.runtime.sendMessage', 'messaging',
        reply && reply.type === 'hello-ack' && typeof reply.tabId === 'number'
          ? L.works({tabId: reply.tabId}) : L.fails('reply: ' + L.shape(reply)));
    } catch (e) { recFail('msg.runtime.sendMessage', 'messaging', e); }

    try {
      rec('msg.runtime.connect', 'messaging', await portCheck());
    } catch (e) { recFail('msg.runtime.connect', 'messaging', e); }

    var form = null;
    try {
      form = fill('ass-user');
      var seen = await waitAttr(form, 'data-ass-seen-user', 'ass-user', 2000);
      rec('pm.fill.main_frame', 'password_manager', seen === true ? L.works('page saw ass-user')
        : L.fails('data-ass-seen-user was ' + JSON.stringify(seen)));
    } catch (e) { recFail('pm.fill.main_frame', 'password_manager', e); }

    try {
      if (!form) throw new Error('no form: fill failed');
      form.requestSubmit();
      var sub = await waitAttr(form, 'data-ass-submitted', 'ok:ass-user', 5000);
      rec('pm.submit.main_frame', 'password_manager', sub === true ? L.works('page marked ok:ass-user')
        : L.fails('data-ass-submitted was ' + JSON.stringify(sub)));
    } catch (e) { recFail('pm.submit.main_frame', 'password_manager', e); }
  }

  async function crossFrame() {
    try {
      var form = fill('ass-frame-user');
      var seen = await waitAttr(form, 'data-ass-seen-user', 'ass-frame-user', 2000);
      rec('pm.fill.iframe_cross_origin', 'password_manager', seen === true ? L.works('frame saw ass-frame-user')
        : L.fails('data-ass-seen-user was ' + JSON.stringify(seen)));
    } catch (e) { recFail('pm.fill.iframe_cross_origin', 'password_manager', e); }
  }

  // The victim page only announces itself; nothing is recorded.
  if (top_ && path === '/victim.html') {
    try { chrome.runtime.sendMessage({type: 'victim-hello'}).catch(function () {}); } catch (e) {}
    return;
  }

  if (!kind || !(top_ ? path === '/start.html' : path === '/frame.html')) return;

  // Answer the worker's ping on every load, before the hello goes out.
  if (top_) {
    chrome.runtime.onMessage.addListener(function (msg, sender, sendResponse) {
      if (msg && msg.type === 'worker-ping') sendResponse({type: 'cs-pong', url: location.href});
    });
  }

  if (!L.once('cs_idle:' + kind)) return;  // not again after tabs.reload (lib.js, once)

  try {
    var state = document.readyState;
    rec(ids[kind], 'content_scripts', state === 'interactive' || state === 'complete'
      ? L.works({readyState: state}) : L.fails({readyState: state}));
  } catch (e) { recFail(ids[kind], 'content_scripts', e); }

  if (top_) mainFrame();
  else if (kind === 'iframe_cross_origin') crossFrame();
})();
