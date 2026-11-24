// Content script at document_start. Records only on start.html (main frame) and frame.html (frames).
(function () {
  var L = self.assLib;
  var top_ = window === top;
  var kind = top_ ? 'main_frame'
    : location.hostname === '127.0.0.1' ? 'iframe_same_origin'
    : location.hostname === 'localhost' ? 'iframe_cross_origin' : null;
  var ids = {
    main_frame: 'cs.document_start.main_frame',
    iframe_same_origin: 'cs.document_start.iframe_same_origin',
    iframe_cross_origin: 'cs.document_start.iframe_cross_origin'
  };
  var path = location.pathname;
  if (!kind || !(top_ ? path === '/start.html' : path === '/frame.html')) return;
  if (!L.once('cs_start:' + kind)) return;  // not again after tabs.reload (lib.js, once)
  try {
    var state = document.readyState;
    L.record({
      id: ids[kind], group: 'content_scripts',
      result: state === 'loading' ? 'works' : 'fails',
      detail: {readyState: state}
    });
  } catch (e) {
    L.record({id: ids[kind], group: 'content_scripts', result: 'fails', detail: L.errMsg(e)});
  }
})();
