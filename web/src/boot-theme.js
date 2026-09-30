// Loaded synchronously in <head>: apply the stored theme before first paint (no flash).
// Pack themes (features/themes.js) also need styles/themes-extra.css. The link is render-blocking
// where `blocking` is supported; elsewhere themes.css paints porcelain / graphite for those few
// milliseconds, so the first frame is at least the right light or dark.
(function () {
  var id = 'auto';
  try { id = JSON.parse(localStorage.getItem('ferro.theme')) || 'auto'; } catch (e) { /* default */ }
  var base = ['graphite', 'porcelain', 'carbon'];
  var pack = ['slate', 'fjord', 'abyss', 'moss', 'umber', 'dusk', 'paper', 'mist', 'sand', 'sage', 'frost', 'dawn', 'chalk'];
  if (id === 'auto' || (base.indexOf(id) < 0 && pack.indexOf(id) < 0)) {
    id = window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches ? 'porcelain' : 'graphite';
  }
  if (pack.indexOf(id) >= 0) {
    var link = document.createElement('link');
    link.id = 'theme-pack';
    link.rel = 'stylesheet';
    link.setAttribute('blocking', 'render');
    link.href = 'styles/themes-extra.css';
    document.head.appendChild(link);
  }
  document.documentElement.setAttribute('data-theme', id);
})();
