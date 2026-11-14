// chestcraft viewer (?view): what scripts add to a page that works without them.
// - Pictures: a click (mouse, pen, Enter) or a double tap switches between "fit" and 100 %,
//   centered on the point; dragging pans while zoomed; two fingers pinch. 100 % means the
//   original's pixels: the original is fetched for that (data-full), the 2048 px picture
//   stands in until it has arrived. Esc returns to fit before keys.js may close the viewer.
// - Swipe left/right on touch steps to the next/previous file (not while zoomed).
// - The next file's picture is fetched ahead (data-preload on the "next" arrow).
// - Info: the toggle opens/closes the panel in place; the address and the previous/next
//   links follow, so the panel stays as it is while stepping through the folder.
// - Delete asks first (the dialog); without scripts the form posts straight away.
// Nothing animates (zoom and paging jump), so there is no motion to reduce.
// Sizes go through the picture's width/height attributes and the stage's scroll position:
// no inline styles (the page's CSP has no 'unsafe-inline' for styles).
(() => {
  'use strict';
  const js = (sel, root = document) => root.querySelector(`[data-js~="${sel}"]`);
  const viewer = js('viewer');
  if (!viewer) return;
  const stage = js('viewer-stage', viewer);
  const prev = js('viewer-prev', viewer);
  const next = js('viewer-next', viewer);

  // --- info panel -------------------------------------------------------------------------
  const infoBtn = js('viewer-info', viewer);
  const panel = js('viewer-info-panel', viewer);
  const withInfo = (href, open) => {
    const u = new URL(href, location.href);
    if (open) u.searchParams.set('info', '1');
    else u.searchParams.delete('info');
    // URLSearchParams writes `view=`; the server reads both, keep the address as it was.
    return u.pathname + u.search.replace(/([?&])view=(&|$)/, '$1view$2') + u.hash;
  };
  if (infoBtn && panel) {
    infoBtn.addEventListener('click', (e) => {
      e.preventDefault();
      const open = panel.hidden;
      panel.hidden = !open;
      infoBtn.setAttribute('aria-expanded', String(open));
      for (const a of [prev, next]) if (a) a.href = withInfo(a.getAttribute('href'), open);
      const here = location.pathname + location.search;
      try { history.replaceState(history.state, '', withInfo(here, open)); } catch (_) { /* file: or sandbox */ }
      infoBtn.href = withInfo(here, !open);
      fitAgain();
    });
  }

  // --- delete asks first ------------------------------------------------------------------
  const delForm = js('viewer-delete', viewer);
  const delDialog = js('viewer-delete-dialog', viewer);
  if (delForm && delDialog) {
    delForm.addEventListener('submit', (e) => {
      e.preventDefault();
      delDialog.returnValue = '';
      delDialog.showModal();
    });
    delDialog.addEventListener('close', () => {
      if (delDialog.returnValue === 'delete') delForm.submit();
    });
  }

  // --- preload the next picture -----------------------------------------------------------
  if (next && next.dataset.preload) {
    const load = () => { const i = new Image(); i.decoding = 'async'; i.src = next.dataset.preload; };
    if ('requestIdleCallback' in window) requestIdleCallback(load, { timeout: 2000 });
    else setTimeout(load, 300);
  }

  // --- swipe (touch, not zoomed) ----------------------------------------------------------
  const go = (a) => { if (a) a.click(); };
  let swipe = null;
  if (stage) {
    stage.addEventListener('pointerdown', (e) => {
      swipe = e.pointerType === 'touch' && e.isPrimary && !zoomed()
        ? { x: e.clientX, y: e.clientY, t: e.timeStamp, id: e.pointerId }
        : null;
    });
    stage.addEventListener('pointerup', (e) => {
      const s = swipe;
      swipe = null;
      if (!s || s.id !== e.pointerId || zoomed() || pinch) return;
      const dx = e.clientX - s.x;
      const dy = e.clientY - s.y;
      if (Math.abs(dx) > 60 && Math.abs(dx) > 1.5 * Math.abs(dy) && e.timeStamp - s.t < 800) {
        go(dx < 0 ? next : prev);
      }
    });
    stage.addEventListener('pointercancel', () => { swipe = null; });
  }

  // --- zoom ---------------------------------------------------------------------------------
  const img = stage && js('viewer-image', stage);
  const link = stage && js('viewer-zoom', stage);
  let scale = 0; // 0 = fit; else the original's pixels per CSS pixel
  let natural = null; // [w, h] of the original (100 %)
  let fullLoaded = false;
  let pinch = null;
  const zoomed = () => scale > 0;

  function measure() {
    if (!img || !img.naturalWidth) return null;
    const w = Number(img.dataset.w) || 0;
    const h = Number(img.dataset.h) || 0;
    // The original's header gave its size; else the picture shown is the largest there is.
    return w && h ? [w, h] : [img.naturalWidth, img.naturalHeight];
  }
  // The scale "fit" shows the picture at (≤ 1: never enlarged).
  function fitScale() {
    if (!natural) return 1;
    const pad = 2 * parseFloat(getComputedStyle(stage).paddingLeft || '0');
    const w = (stage.clientWidth - pad) / natural[0];
    const h = (stage.clientHeight - pad) / natural[1];
    return Math.min(1, w, h);
  }
  function markZoomable() {
    natural = measure();
    if (!natural) return;
    if (fitScale() < 0.98) stage.dataset.zoomable = '';
    else delete stage.dataset.zoomable;
  }
  function fitAgain() {
    if (!img) return;
    requestAnimationFrame(() => { if (!zoomed()) markZoomable(); });
  }

  // Show the picture at `s`, keeping the picture point under (cx, cy) where it is.
  function setScale(s, cx, cy) {
    const fit = fitScale();
    if (s <= fit * 1.02) { toFit(); return; }
    const r = img.getBoundingClientRect();
    const st = stage.getBoundingClientRect();
    const px = cx === undefined ? st.left + st.width / 2 : cx;
    const py = cy === undefined ? st.top + st.height / 2 : cy;
    const fx = Math.min(1, Math.max(0, (px - r.left) / r.width));
    const fy = Math.min(1, Math.max(0, (py - r.top) / r.height));
    scale = s;
    stage.dataset.zoom = '';
    img.width = Math.round(natural[0] * s);
    img.height = Math.round(natural[1] * s);
    // After layout: scroll so the same point sits under the pointer.
    const w = img.width;
    const h = img.height;
    stage.scrollLeft = fx * w - (px - st.left);
    stage.scrollTop = fy * h - (py - st.top);
    if (img.dataset.full && !fullLoaded) loadFull();
  }
  function toFit() {
    scale = 0;
    delete stage.dataset.zoom;
    img.removeAttribute('width');
    img.removeAttribute('height');
    stage.scrollLeft = 0;
    stage.scrollTop = 0;
  }
  function loadFull() {
    fullLoaded = true;
    const full = new Image();
    full.decoding = 'async';
    full.addEventListener('load', () => {
      img.removeAttribute('srcset');
      img.src = full.src;
      if (!(Number(img.dataset.w) && Number(img.dataset.h))) {
        // The header didn't say: now the original does. Keep the view's center.
        const st = stage.getBoundingClientRect();
        const old = natural;
        natural = [full.naturalWidth, full.naturalHeight];
        if (zoomed()) setScale((scale * old[0]) / natural[0], st.left + st.width / 2, st.top + st.height / 2);
      }
    });
    full.src = img.dataset.full;
  }
  function toggle(cx, cy) {
    if (!natural) markZoomable();
    if (!natural) return;
    if (zoomed()) toFit();
    else if (stage.dataset.zoomable !== undefined) setScale(1, cx, cy);
  }

  if (img) {
    if (img.complete) markZoomable();
    img.addEventListener('load', () => { if (!zoomed()) markZoomable(); });
    addEventListener('resize', () => { if (zoomed()) { if (scale < fitScale()) toFit(); } else markZoomable(); });

    let lastType = 'mouse';
    let lastTap = null;
    let drag = null;
    const points = new Map();

    stage.addEventListener('pointerdown', (e) => {
      lastType = e.pointerType;
      points.set(e.pointerId, [e.clientX, e.clientY]);
      if (points.size === 2 && natural) {
        // Two fingers: pinch from here.
        const [a, b] = [...points.values()];
        const r = img.getBoundingClientRect();
        pinch = {
          dist: Math.hypot(a[0] - b[0], a[1] - b[1]) || 1,
          start: zoomed() ? scale : r.width / natural[0],
        };
        drag = null;
        lastTap = null;
        return;
      }
      if (zoomed() && e.isPrimary && (e.pointerType !== 'mouse' || e.button === 0)) {
        e.preventDefault(); // no native drag of the link or the picture, no text selection
        drag = { x: e.clientX, y: e.clientY, sl: stage.scrollLeft, st: stage.scrollTop, moved: false };
        stage.setPointerCapture(e.pointerId);
      }
    });
    stage.addEventListener('pointermove', (e) => {
      if (!points.has(e.pointerId)) return;
      points.set(e.pointerId, [e.clientX, e.clientY]);
      if (pinch && points.size === 2) {
        const [a, b] = [...points.values()];
        const d = Math.hypot(a[0] - b[0], a[1] - b[1]);
        const s = Math.min(4, (pinch.start * d) / pinch.dist);
        if (s > fitScale() * 1.02 || zoomed()) setScale(s, (a[0] + b[0]) / 2, (a[1] + b[1]) / 2);
        return;
      }
      if (drag) {
        const dx = e.clientX - drag.x;
        const dy = e.clientY - drag.y;
        if (Math.abs(dx) + Math.abs(dy) > 4) {
          drag.moved = true;
          stage.dataset.drag = '';
        }
        stage.scrollLeft = drag.sl - dx;
        stage.scrollTop = drag.st - dy;
      }
    });
    const up = (e) => {
      points.delete(e.pointerId);
      if (points.size < 2 && pinch) {
        // A pinch that ended near "fit" settles there.
        if (zoomed() && scale <= fitScale() * 1.05) toFit();
        if (points.size === 0) setTimeout(() => { pinch = null; }, 0);
      }
      if (drag) {
        delete stage.dataset.drag;
        if (drag.moved) stage.ccDragged = true;
        drag = null;
      }
      // Double tap: two taps close together in time and place.
      if (e.type === 'pointerup' && e.pointerType === 'touch' && !pinch) {
        const t = { x: e.clientX, y: e.clientY, t: e.timeStamp };
        if (lastTap && t.t - lastTap.t < 320 && Math.hypot(t.x - lastTap.x, t.y - lastTap.y) < 30) {
          lastTap = null;
          if (e.target === img || zoomed()) toggle(t.x, t.y);
        } else {
          lastTap = t;
        }
      }
    };
    stage.addEventListener('pointerup', up);
    stage.addEventListener('pointercancel', up);

    // The link to the original stays for pages without scripts; here a click on the
    // picture zooms in, and any click on the stage zooms out again (while panning, the
    // pointer is captured by the stage, so the click lands there). Touch toggles by double
    // tap only (a single tap does nothing).
    stage.addEventListener('click', (e) => {
      if (link && link.contains(e.target)) e.preventDefault();
      if (stage.ccDragged) { stage.ccDragged = false; return; }
      if (e.detail === 0) { if (link && link.contains(e.target)) toggle(); return; } // Enter
      if (lastType === 'touch') return;
      if (zoomed()) toFit();
      else if (e.target === img) toggle(e.clientX, e.clientY);
    });
    // A double click is two clicks (in and out again); it selects nothing.
    stage.addEventListener('dblclick', (e) => e.preventDefault());
  }

  // Esc leaves 100 % first; only then may keys.js close the viewer. Capture phase, so this
  // runs before keys.js's listener whatever the script order.
  addEventListener(
    'keydown',
    (e) => {
      if (e.key === 'Escape' && zoomed() && !document.querySelector('dialog[open]')) {
        e.preventDefault();
        e.stopImmediatePropagation();
        toFit();
      }
    },
    true,
  );
})();
