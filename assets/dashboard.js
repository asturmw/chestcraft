// chestcraft dashboard: reload the dashboard part of the page every data-refresh seconds
// while the tab is visible, and the crosshair of the activity charts. Without JS the page
// stays as it was rendered and its "as of" label tells how old it is.
(() => {
  'use strict';
  const root = document.getElementById('dashboard');
  if (!root) return;
  const every = (Number(root.dataset.refresh) || 30) * 1000;
  let busy = false;

  // ---- Activity charts: one moment for all of them ----------------------------------
  // The four charts share one time grid (data-start, data-step, data-n on the section).
  // Pointing at one, or arrow keys on a focused one, picks a bucket; every chart moves its
  // crosshair there and shows its values then, and the header says when. Leaving puts
  // "now" back.

  let picked = null; // bucket index, or null for "now"
  const parsed = new WeakMap();

  function seriesOf(fig) {
    let s = parsed.get(fig);
    if (!s) {
      const col = (name) => (fig.dataset[name] || '').split(',').map((v) => (v === '' ? null : Number(v)));
      s = { a: col('a'), b: col('b'), max: Number(fig.dataset.max) || 1, unit: fig.dataset.unit };
      parsed.set(fig, s);
    }
    return s;
  }

  function format(sec, unit, v) {
    if (v === null || v === undefined || !Number.isFinite(v)) return '–';
    const dec = sec.dataset.decimal || '.';
    if (unit === 'percent') return `${(v < 9.95 ? v.toFixed(1) : v.toFixed(0)).replace('.', dec)} %`;
    let size;
    if (v < 1000) size = `${Math.max(0, Math.round(v))} B`;
    else {
      const units = ['B', 'kB', 'MB', 'GB', 'TB', 'PB'];
      let x = v;
      let u = 0;
      while (x >= 1000 && u < units.length - 1) { x /= 1000; u += 1; }
      size = `${(x < 100 ? x.toFixed(1) : x.toFixed(0)).replace('.', dec)} ${units[u]}`;
    }
    return unit === 'rate' ? (sec.dataset.rate || '{size}/s').replace('{size}', size) : size;
  }

  function when(sec, i) {
    const t = (Number(sec.dataset.start) + i * Number(sec.dataset.step)) * 1000;
    // A day's bucket (90 days) is named by its date; finer ones by their start time too.
    const opts = sec.dataset.range === '90d' ? {} : { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' };
    if (sec.dataset.range !== '24h') Object.assign(opts, { weekday: 'short', day: 'numeric', month: 'short' });
    if (sec.dataset.tz) opts.timeZone = sec.dataset.tz;
    try {
      return new Intl.DateTimeFormat(sec.dataset.lang || undefined, opts).format(t);
    } catch (e) {
      delete opts.timeZone;
      return new Intl.DateTimeFormat(undefined, opts).format(t);
    }
  }

  function show(sec, i) {
    const n = Number(sec.dataset.n) || 1;
    const at = sec.querySelector('[data-js="pulse-at"]');
    const label = i === null ? '' : when(sec, i);
    if (i === null) {
      if (at) at.textContent = at.dataset.now;
      delete sec.dataset.state;
    } else {
      if (at) at.textContent = label;
      sec.dataset.state = 'scrub';
    }
    // Each chart's own copy, for the one-column layout where the header is out of sight.
    for (const w of sec.querySelectorAll('[data-js="pulse-when"]')) w.textContent = label;
    const x = i === null ? '0' : `${(((i + 0.5) / n) * 100).toFixed(3)}%`;
    for (const fig of sec.querySelectorAll('figure[data-unit]')) {
      const s = seriesOf(fig);
      const cross = fig.querySelector('[data-js="pulse-cross"]');
      if (cross) {
        cross.setAttribute('x1', x);
        cross.setAttribute('x2', x);
        cross.setAttribute('visibility', i === null ? 'hidden' : 'visible');
      }
      for (const dot of fig.querySelectorAll('[data-js="pulse-dot"]')) {
        const v = i === null ? null : s[dot.dataset.slot][i];
        if (v === null || v === undefined) { dot.setAttribute('visibility', 'hidden'); continue; }
        const y = 100 - Math.min(1, Math.max(0, v / s.max)) * 100;
        dot.setAttribute('cx', x);
        dot.setAttribute('cy', `${y.toFixed(2)}%`);
        dot.setAttribute('visibility', 'visible');
      }
      for (const val of fig.querySelectorAll('[data-js="pulse-val"]')) {
        const v = i === null ? undefined : s[val.dataset.slot][i];
        const text = i === null ? val.dataset.now : format(sec, s.unit, v);
        val.textContent = text;
        if (text === '–') val.dataset.state = 'empty';
        else delete val.dataset.state;
      }
    }
  }

  function bucketAt(sec, plot, clientX) {
    const r = (plot.querySelector('svg') || plot).getBoundingClientRect();
    const n = Number(sec.dataset.n) || 1;
    return Math.max(0, Math.min(n - 1, Math.floor(((clientX - r.left) / r.width) * n)));
  }

  function pick(sec, i) {
    picked = i;
    show(sec, i);
  }

  const plotOf = (el) => el && el.closest && el.closest('[data-js="pulse-plot"]');

  root.addEventListener('pointermove', (e) => {
    const plot = plotOf(e.target);
    if (!plot) return;
    const sec = plot.closest('[data-js="activity"]');
    pick(sec, bucketAt(sec, plot, e.clientX));
  });
  root.addEventListener('pointerdown', (e) => {
    const plot = plotOf(e.target);
    if (!plot) return;
    const sec = plot.closest('[data-js="activity"]');
    pick(sec, bucketAt(sec, plot, e.clientX));
  });
  root.addEventListener('pointerout', (e) => {
    const plot = plotOf(e.target);
    if (!plot || plot.contains(e.relatedTarget) || e.pointerType === 'touch') return;
    if (document.activeElement === plot) return;
    pick(plot.closest('[data-js="activity"]'), null);
  });
  root.addEventListener('focusout', (e) => {
    const plot = plotOf(e.target);
    if (plot && !plotOf(e.relatedTarget)) pick(plot.closest('[data-js="activity"]'), null);
  });
  root.addEventListener('keydown', (e) => {
    const plot = plotOf(e.target);
    if (!plot || e.altKey || e.ctrlKey || e.metaKey) return;
    const sec = plot.closest('[data-js="activity"]');
    const n = Number(sec.dataset.n) || 1;
    const big = Math.max(1, Math.round(n / 24));
    const from = picked === null ? n - 1 : picked;
    let i;
    if (e.key === 'ArrowLeft') i = from - (e.shiftKey ? big : 1);
    else if (e.key === 'ArrowRight') i = from + (e.shiftKey ? big : 1);
    else if (e.key === 'Home') i = 0;
    else if (e.key === 'End') i = n - 1;
    else if (e.key === 'Escape') { pick(sec, null); return; }
    else return;
    e.preventDefault();
    pick(sec, Math.max(0, Math.min(n - 1, i)));
  });

  // ---- Refresh ----------------------------------------------------------------------

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
      // Keep keyboard focus on the chart the reader is in.
      const focusKind = plotOf(document.activeElement)?.closest('figure')?.dataset.kind;
      root.replaceChildren(...fresh.childNodes);
      for (const a of ['data-verdict']) root.setAttribute(a, fresh.getAttribute(a) || '');
      delete root.dataset.state;
      if (focusKind) {
        root.querySelector(`figure[data-kind="${CSS.escape(focusKind)}"] [data-js="pulse-plot"]`)?.focus({ preventScroll: true });
      }
      const sec = root.querySelector('[data-js="activity"]');
      if (sec && picked !== null) {
        // The grid moved on by the time between the two renders.
        const n = Number(sec.dataset.n) || 1;
        pick(sec, Math.max(0, Math.min(n - 1, picked)));
      } else picked = null;
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
