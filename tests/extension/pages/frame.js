// frame.html: the same page-world listener as start.html, nothing else.
(function () {
  var form = document.getElementById('login-form');
  var user = document.getElementById('login-user');
  function seen() { form.setAttribute('data-ass-seen-user', user.value); }
  user.addEventListener('input', seen);
  user.addEventListener('change', seen);
  form.addEventListener('submit', function (ev) { ev.preventDefault(); });
})();
