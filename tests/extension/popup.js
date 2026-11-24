// Runs as an ordinary tab in the content view, loaded by the run script through the debug port.
(async function () {
  var L = self.assLib;
  var T = self.ASS_TEST;
  function rec(id, group, r) { L.record({id: id, group: group, result: r.result, detail: r.detail}); }
  function recFail(id, group, e) { L.record({id: id, group: group, result: 'fails', detail: L.errMsg(e)}); }

  try {
    rec('page.popup.loads', 'extension_pages', L.works(location.href));
  } catch (e) { recFail('page.popup.loads', 'extension_pages', e); }

  try {
    var d = {
      runtimeId: chrome.runtime.id,
      storage: typeof chrome.storage.local,
      tabs: typeof chrome.tabs,
      action: typeof chrome.action
    };
    var ok = d.runtimeId === T.extensionId && d.storage === 'object' && d.tabs === 'object' && d.action === 'object';
    rec('page.popup.chrome_apis', 'extension_pages', ok ? L.works(d) : L.fails(d));
  } catch (e) { recFail('page.popup.chrome_apis', 'extension_pages', e); }

  try {
    var got = await chrome.storage.local.get('fromWorker');
    rec('page.popup.storage_get', 'extension_pages', got.fromWorker === 'set-by-worker'
      ? L.works(got.fromWorker) : L.fails('got ' + L.shape(got)));
  } catch (e) { recFail('page.popup.storage_get', 'extension_pages', e); }

  try {
    var reply = await L.withTimeout(chrome.runtime.sendMessage({type: 'popup-ping'}), 5000);
    rec('page.popup.runtime_sendMessage', 'extension_pages', reply && reply.type === 'popup-pong'
      ? L.works(reply) : L.fails('reply: ' + L.shape(reply)));
  } catch (e) { recFail('page.popup.runtime_sendMessage', 'extension_pages', e); }

  try {
    var tab = await chrome.tabs.getCurrent();
    rec('tabs.getCurrent.extension_page', 'tabs', tab && tab.url && tab.url.indexOf('popup.html') >= 0
      ? L.works({tabId: tab.id}) : L.fails('resolved ' + L.shape(tab)));
  } catch (e) { recFail('tabs.getCurrent.extension_page', 'tabs', e); }

  L.record({id: 'ctl.popup.done', group: 'control', result: 'works'});
})();
