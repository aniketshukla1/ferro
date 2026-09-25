// Loaded synchronously in <head>: apply the stored theme before first paint (no flash).
(function () {
  var id = 'auto';
  try { id = JSON.parse(localStorage.getItem('ferro.theme')) || 'auto'; } catch (e) { /* default */ }
  var known = ['graphite', 'porcelain', 'carbon'];
  if (id === 'auto' || known.indexOf(id) < 0) {
    id = window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches ? 'porcelain' : 'graphite';
  }
  document.documentElement.setAttribute('data-theme', id);
})();
