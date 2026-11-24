// Waits for the worker's boot record, then moves on to start.html.
(async function () {
  var t0 = Date.now();
  var saw = await assPage.waitFor('ctl.worker.boot', 20000);
  await assPage.record({id: 'ctl.boot.page', group: 'control', result: 'works',
    detail: {waitedMs: Date.now() - t0, sawWorker: saw}});
  location.replace('/start.html');
})();
