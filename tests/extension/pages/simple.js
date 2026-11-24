// Records the control id named by data-ass-control on <html> when the page loads.
window.addEventListener('load', function () {
  var id = document.documentElement.getAttribute('data-ass-control');
  assPage.record({id: id, group: 'control', result: 'works', detail: location.href});
});
