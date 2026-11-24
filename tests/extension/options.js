// Runs as an ordinary tab in the content view, loaded by the run script through the debug port.
(async function () {
  var L = self.assLib;
  var T = self.ASS_TEST;
  function rec(id, group, r) { L.record({id: id, group: group, result: r.result, detail: r.detail}); }
  function recFail(id, group, e) { L.record({id: id, group: group, result: 'fails', detail: L.errMsg(e)}); }

  try {
    rec('page.options.loads', 'extension_pages', L.works(location.href));
  } catch (e) { recFail('page.options.loads', 'extension_pages', e); }

  try {
    var d = {runtimeId: chrome.runtime.id, storage: typeof chrome.storage.local};
    rec('page.options.chrome_apis', 'extension_pages',
      d.runtimeId === T.extensionId && d.storage === 'object' ? L.works(d) : L.fails(d));
  } catch (e) { recFail('page.options.chrome_apis', 'extension_pages', e); }

  L.record({id: 'ctl.options.done', group: 'control', result: 'works'});
})();
