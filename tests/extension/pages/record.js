// Page-world recorder. Pages are not extension code, so they cannot use lib.js.
(function () {
  var launch = null;
  var launchReady = fetch(location.origin + '/__ass/launch', {cache: 'no-store'})
    .then(function (r) { return r.json(); })
    .then(function (j) { launch = j.launch; })
    .catch(function () {});

  function record(rec) {
    rec = rec || {};
    rec.by = 'page';
    rec.t = Date.now();
    return launchReady.then(function () {
      rec.launch = launch;
      try {
        var key = 'ass-records:page:' + location.pathname;
        var list = JSON.parse(localStorage.getItem(key) || '[]');
        list.push(rec);
        localStorage.setItem(key, JSON.stringify(list));
      } catch (e) {}
      return fetch(location.origin + '/__ass/record', {
        method: 'POST',
        headers: {'Content-Type': 'text/plain'},
        body: JSON.stringify(rec),
        keepalive: true
      }).then(function () {}, function () {});
    });
  }

  function has(id) {
    return fetch(location.origin + '/__ass/has?id=' + encodeURIComponent(id), {cache: 'no-store'})
      .then(function (r) { return r.json(); })
      .then(function (j) { return j.has === true; })
      .catch(function () { return false; });
  }

  function waitFor(id, ms) {
    var end = Date.now() + ms;
    return new Promise(function (resolve) {
      (function poll() {
        has(id).then(function (ok) {
          if (ok) return resolve(true);
          if (Date.now() >= end) return resolve(false);
          setTimeout(poll, 500);
        });
      })();
    });
  }

  // once(name): true the first time in this tab and origin, false after a reload (as lib.js).
  function once(name) {
    try {
      var key = 'ass-once:' + name;
      if (sessionStorage.getItem(key)) return false;
      sessionStorage.setItem(key, '1');
    } catch (e) {}
    return true;
  }

  window.assPage = {record: record, has: has, waitFor: waitFor, once: once};
})();
