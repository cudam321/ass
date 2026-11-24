// Shared by the worker, the content scripts and the extension pages.
// Injected twice into one content-script world, so: no top-level let, const or class.
self.assLib = self.assLib || (function () {
  var T = self.ASS_TEST || {server: null, xserver: null, extensionId: '', launch: 0, risky: true, skip: []};
  var isWorker = typeof ServiceWorkerGlobalScope !== 'undefined' && self instanceof ServiceWorkerGlobalScope;
  var isExtPage = !isWorker && location.protocol === 'chrome-extension:';
  var by;
  if (isWorker) {
    by = 'worker';
  } else if (isExtPage) {
    by = location.pathname.split('/').pop().replace(/\.html$/, '');
  } else {
    by = 'content';
  }
  var contextId = by + '-' + Math.random().toString(36).slice(2, 10);
  var L = {bootId: null, by: by, isWorker: isWorker};

  function noop() {}
  function errMsg(e) { return e && e.message ? String(e.message) : String(e); }
  function sleep(ms) { return new Promise(function (r) { setTimeout(r, ms); }); }

  function withTimeout(promise, ms, label) {
    return new Promise(function (resolve, reject) {
      var timer = setTimeout(function () {
        reject(new Error('no result within ' + ms + ' ms' + (label ? ' (' + label + ')' : '')));
      }, ms);
      Promise.resolve(promise).then(function (v) { clearTimeout(timer); resolve(v); },
                                    function (e) { clearTimeout(timer); reject(e); });
    });
  }

  function works(detail) { return {result: 'works', detail: detail}; }
  function fails(detail) { return {result: 'fails', detail: detail}; }
  function absent(detail) { return {result: 'absent', detail: detail}; }
  function notRun(reason) { return {result: 'not-run', detail: reason}; }

  function absentError(msg) {
    var e = new Error(msg);
    e.absent = true;
    return e;
  }

  // Walks a path such as 'chrome.tabs.group' from self. Returns the value or undefined.
  function resolve(path) {
    var obj = self;
    var parts = path.split('.');
    for (var i = 0; i < parts.length; i++) {
      if (obj === undefined || obj === null) return undefined;
      obj = obj[parts[i]];
    }
    return obj;
  }

  // The function at that path, bound to its namespace. Throws an error flagged absent if undefined.
  function api(path) {
    var cut = path.lastIndexOf('.');
    var ns = resolve(path.slice(0, cut));
    var fn = ns ? ns[path.slice(cut + 1)] : undefined;
    if (typeof fn !== 'function') throw absentError(path + ' is undefined');
    return fn.bind(ns);
  }

  function short(v) {
    var s;
    try { s = JSON.stringify(v); } catch (e) { s = String(v); }
    return s === undefined ? 'undefined' : s.slice(0, 60);
  }

  function shape(v) {
    var s;
    try {
      if (v === undefined) s = 'undefined';
      else if (v === null) s = 'null';
      else if (Array.isArray(v)) s = 'array[' + v.length + ']' + (v.length ? ' first=' + short(v[0]) : '');
      else if (typeof v === 'object') {
        var keys = Object.keys(v);
        s = 'object{' + keys.slice(0, 12).join(',') + '}';
        var first = [];
        for (var i = 0; i < keys.length && first.length < 4; i++) {
          var x = v[keys[i]];
          if (x === null || typeof x !== 'object') first.push(keys[i] + '=' + short(x));
        }
        if (first.length) s += ' ' + first.join(' ');
      } else s = typeof v + ':' + String(v).slice(0, 100);
    } catch (e) { s = 'unshapeable'; }
    return s.slice(0, 300);
  }

  // Path 2: a copy inside the browser that the run script reads later through the debug port.
  function store(rec) {
    if (isWorker) {
      try { (self.__assRecords = self.__assRecords || []).push(rec); } catch (e) {}
    } else if (isExtPage) {
      try { (window.__assRecords = window.__assRecords || []).push(rec); } catch (e) {}
      try {
        var pre = document.getElementById('ass-records');
        if (pre) pre.appendChild(document.createTextNode(JSON.stringify(rec) + '\n'));
      } catch (e) {}
    } else {
      try {
        var key = 'ass-records:' + contextId;
        var list = JSON.parse(localStorage.getItem(key) || '[]');
        list.push(rec);
        localStorage.setItem(key, JSON.stringify(list));
      } catch (e) {}
      try { if (window !== top) parent.postMessage({assRecord: rec}, '*'); } catch (e) {}
    }
  }

  // Path 1: HTTP post to the page server. Never rejects.
  function post(rec) {
    var base = T.server;
    if (!isWorker && !isExtPage && location.hostname === 'localhost') base = T.xserver || T.server;
    if (!base) return Promise.resolve();
    try {
      return fetch(base + '/__ass/record', {
        method: 'POST',
        headers: {'Content-Type': 'text/plain'},
        body: JSON.stringify(rec),
        keepalive: true
      }).then(noop, noop);
    } catch (e) {
      return Promise.resolve();
    }
  }

  function record(rec) {
    rec = rec || {};
    rec.by = by;
    rec.launch = T.launch;
    rec.t = Date.now();
    if (isWorker && L.bootId) rec.boot = L.bootId;
    store(rec);
    return post(rec);
  }

  function skipped(id) { return !!(T.skip && T.skip.indexOf(id) >= 0); }

  // One worker-suite step: begin marker, the call, the result.
  function check(id, group, fn, opts) {
    opts = opts || {};
    if (skipped(id)) return Promise.resolve(null);
    return (async function () {
      var res;
      if (opts.risky && T.risky === false) {
        res = notRun('risky checks switched off (ASS_EXT_TEST_RISKY=0)');
      } else {
        // The marker must leave the process before the call, in case the call kills the browser.
        await withTimeout(record({id: id, group: group, phase: 'begin'}), 2000, 'begin marker').catch(noop);
        var ms = opts.timeoutMs || 8000;
        try {
          res = await withTimeout(Promise.resolve().then(fn), ms);
          if (!res || !res.result) res = fails('check returned no result object');
        } catch (e) {
          res = e && e.absent ? absent(errMsg(e)) : fails(errMsg(e));
        }
      }
      var detail = res.detail;
      if (typeof detail === 'string') detail = detail.slice(0, 500);
      await withTimeout(record({id: id, group: group, result: res.result, detail: detail}), 2000, 'result').catch(noop);
      return res;
    })();
  }

  // true, false, or null when the server cannot be reached.
  function hasRaw(id) {
    if (!T.server) return Promise.resolve(null);
    return withTimeout(fetch(T.server + '/__ass/has?id=' + encodeURIComponent(id), {cache: 'no-store'})
      .then(function (r) { return r.json(); }), 3000)
      .then(function (j) { return j.has === true ? true : j.has === false ? false : null; },
            function () { return null; });
  }
  function has(id) { return hasRaw(id).then(function (v) { return v === true; }); }

  // How many result records with that id reached the server in this launch; null when it cannot be reached.
  function count(id) {
    if (!T.server) return Promise.resolve(null);
    return withTimeout(fetch(T.server + '/__ass/count?id=' + encodeURIComponent(id), {cache: 'no-store'})
      .then(function (r) { return r.json(); }), 3000)
      .then(function (j) { return typeof j.count === 'number' ? j.count : null; },
            function () { return null; });
  }

  // Calling an extension API resets the worker's 30 s idle timer.
  function ping() {
    try {
      var p = chrome.runtime.getPlatformInfo();
      if (p && p.catch) p.catch(noop);
    } catch (e) {}
  }

  async function waitFor(id, ms) {
    var end = Date.now() + ms;
    var lastPing = Date.now();
    for (;;) {
      if (await has(id)) return true;
      if (Date.now() >= end) return false;
      if (isWorker && Date.now() - lastPing >= 10000) { lastPing = Date.now(); ping(); }
      await sleep(500);
    }
  }

  // once(name), in a content script: true the first time in this tab and origin, false after.
  // tabs.reload loads start.html again and tabs.update.url leaves it right after, so a second
  // pass could be cut short and overwrite the first. Session storage survives a reload and is
  // new in every launch's profile. If it cannot be used, true: twice is better than never.
  function once(name) {
    try {
      var key = 'ass-once:' + name;
      if (sessionStorage.getItem(key)) return false;
      sessionStorage.setItem(key, '1');
    } catch (e) {}
    return true;
  }

  L.record = record; L.check = check; L.api = api; L.resolve = resolve; L.shape = shape; L.once = once;
  L.has = has; L.hasRaw = hasRaw; L.count = count; L.waitFor = waitFor; L.sleep = sleep; L.withTimeout = withTimeout;
  L.works = works; L.fails = fails; L.absent = absent; L.notRun = notRun;
  L.absentError = absentError; L.errMsg = errMsg; L.skipped = skipped; L.ping = ping;
  return L;
})();
