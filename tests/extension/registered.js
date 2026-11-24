// Registered at run time by the worker for /nav*. Proves a registered content script ran.
(function () {
  var p = location.pathname;
  if (window !== top || (p !== '/nav.html' && p !== '/nav2.html')) return;
  self.assLib.record({id: 'scripting.registered_script_ran', group: 'scripting', result: 'works', detail: p});
})();
