// chestcraft dashboard: reload the dashboard part of the page every data-refresh seconds
// while the tab is visible. Without JS the page stays as it was rendered and its "as of"
// label tells how old it is.
(() => {
  'use strict';
  const root = document.getElementById('dashboard');
  if (!root) return;
  const every = (Number(root.dataset.refresh) || 30) * 1000;
  let busy = false;

  async function refresh() {
    if (busy || document.hidden) return;
    busy = true;
    try {
      const res = await fetch(location.href, {
        credentials: 'same-origin',
        headers: { 'Accept': 'text/html' },
        cache: 'no-store',
      });
      if (!res.ok) throw new Error(String(res.status));
      const doc = new DOMParser().parseFromString(await res.text(), 'text/html');
      const fresh = doc.getElementById('dashboard');
      if (!fresh) throw new Error('no dashboard');
      // Keep sections the reader collapsed collapsed.
      const closed = new Set(
        [...root.querySelectorAll('details:not([open])')].map((d) => d.className),
      );
      for (const d of fresh.querySelectorAll('details')) {
        if (closed.has(d.className)) d.removeAttribute('open');
      }
      root.replaceChildren(...fresh.childNodes);
      for (const a of ['data-verdict']) root.setAttribute(a, fresh.getAttribute(a) || '');
      delete root.dataset.state;
    } catch (e) {
      root.dataset.state = 'offline';
    } finally {
      busy = false;
    }
  }

  setInterval(refresh, every);
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) refresh();
  });
})();
