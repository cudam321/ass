// start.html: the page the extension works on. External script only (no inline script).
window.assPageSecret = 'page-only';

(function () {
  var form = document.getElementById('login-form');
  var user = document.getElementById('login-user');
  var pass = document.getElementById('login-pass');

  // The page's own listeners: they report what the page saw, as a password manager's fill would be judged.
  function seen() { form.setAttribute('data-ass-seen-user', user.value); }
  user.addEventListener('input', seen);
  user.addEventListener('change', seen);

  form.addEventListener('submit', function (ev) {
    ev.preventDefault();
    fetch('/__ass/login', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({user: user.value, pass: pass.value})
    }).then(function (r) { return r.json(); }).then(function (j) {
      if (j && j.ok === true) form.setAttribute('data-ass-submitted', 'ok:' + user.value);
    }).catch(function () {});
  });

  // The second frame is on another host (localhost), so another site and another renderer process.
  var cross = document.createElement('iframe');
  cross.src = 'http://localhost:' + location.port + '/frame.html';
  cross.width = 300;
  cross.height = 120;
  document.body.appendChild(cross);

  // A frame on another origin cannot be read; its content script posts its records up to here.
  window.addEventListener('message', function (ev) {
    if (!ev.data || !ev.data.assRecord) return;
    try {
      var list = JSON.parse(localStorage.getItem('ass-records:relay') || '[]');
      list.push(ev.data.assRecord);
      localStorage.setItem('ass-records:relay', JSON.stringify(list));
    } catch (e) {}
  });

  function rec(id, result, detail) {
    return assPage.record({id: id, group: id.indexOf('ctl.') === 0 ? 'control' : 'dnr', result: result, detail: detail});
  }

  function errText(e) { return e && e.message ? e.message : String(e); }

  async function dnrPart() {
    var dynamicAdded = await assPage.waitFor('dnr.dynamic.add', 25000);
    var opts = {cache: 'no-store'};

    try {
      var r = await fetch('/__dnr/control.txt', opts);
      var body = (await r.text()).trim();
      await rec('dnr.control_not_blocked', body === 'control' ? 'works' : 'fails', 'status ' + r.status + ', body ' + JSON.stringify(body.slice(0, 60)));
    } catch (e) { await rec('dnr.control_not_blocked', 'fails', errText(e)); }

    try {
      var r2 = await fetch('/__dnr/static-block.txt', opts);
      await rec('dnr.static.block_fetch', 'fails', 'fetch resolved, status ' + r2.status);
    } catch (e) { await rec('dnr.static.block_fetch', 'works', 'fetch rejected: ' + errText(e)); }

    try {
      var how = await new Promise(function (resolve) {
        var s = document.createElement('script');
        s.src = '/__dnr/static-block.js';
        s.onerror = function () { resolve('error'); };
        s.onload = function () { resolve('load'); };
        setTimeout(function () { resolve('timeout'); }, 5000);
        document.head.appendChild(s);
      });
      var ran = typeof window.assStaticScriptRan !== 'undefined';
      if (how === 'error' && !ran) await rec('dnr.static.block_script', 'works', 'error event, script did not run');
      else if (how === 'timeout') await rec('dnr.static.block_script', 'fails', 'neither load nor error within 5 s');
      else await rec('dnr.static.block_script', 'fails', 'event ' + how + ', script ran: ' + ran);
    } catch (e) { await rec('dnr.static.block_script', 'fails', errText(e)); }

    try {
      var r3 = await fetch('/__dnr/static-redirect-src.txt', opts);
      var b3 = (await r3.text()).trim();
      await rec('dnr.static.redirect', b3 === 'redirect-target' ? 'works' : 'fails',
        {body: b3.slice(0, 60), url: r3.url, redirected: r3.redirected});
    } catch (e) { await rec('dnr.static.redirect', 'fails', errText(e)); }

    var note = dynamicAdded ? '' : ' (dnr.dynamic.add never arrived within 25 s; tried anyway)';
    try {
      var r4 = await fetch('/__dnr/dynamic-block.txt', opts);
      await rec('dnr.dynamic.block_fetch', 'fails', 'fetch resolved, status ' + r4.status + note);
    } catch (e) { await rec('dnr.dynamic.block_fetch', 'works', 'fetch rejected: ' + errText(e) + note); }

    await rec('ctl.page.dnr_done', 'works', {dynamicAdded: dynamicAdded});
  }

  // ctl.page.ready on every load (tabs.reload counts it); the checks once per tab.
  window.addEventListener('load', function () {
    rec('ctl.page.ready', 'works', location.href);
    if (assPage.once('dnr')) dnrPart();
  });
})();
